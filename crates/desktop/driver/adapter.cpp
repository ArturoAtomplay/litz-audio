// Copyright (c) Microsoft Corporation. All rights reserved.
// Derived from the Simple Audio Sample in Windows-driver-samples; see LICENSE.

#include <initguid.h>

#include "litzaudio.h"

PVOID __cdecl operator new(size_t Size, POOL_FLAGS Flags, ULONG Tag) {
    return ExAllocatePool2(Flags, Size, Tag);
}

void __cdecl operator delete(PVOID Pointer, size_t Size) {
    UNREFERENCED_PARAMETER(Size);
    if (Pointer != nullptr) {
        ExFreePool(Pointer);
    }
}

namespace {

#pragma code_seg("PAGE")
NTSTATUS Register(_In_ PDEVICE_OBJECT Device, _In_ PIRP Irp, _In_ PRESOURCELIST Resources,
                  _In_ PWSTR Name, _In_ REFCLSID PortClass, _In_ PUNKNOWN Miniport,
                  _Out_ PPORT *Port) {
    PAGED_CODE();
    NTSTATUS status = PcNewPort(Port, PortClass);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = (*Port)->Init(Device, Irp, Miniport, nullptr, Resources);
    if (NT_SUCCESS(status)) {
        status = PcRegisterSubdevice(Device, Name, *Port);
    }
    if (!NT_SUCCESS(status)) {
        (*Port)->Release();
        *Port = nullptr;
    }
    return status;
}

NTSTATUS StartDevice(_In_ PDEVICE_OBJECT Device, _In_ PIRP Irp, _In_ PRESOURCELIST Resources) {
    PAGED_CODE();
    PUNKNOWN topology = nullptr;
    PUNKNOWN wave = nullptr;
    PPORT topologyPort = nullptr;
    PPORT wavePort = nullptr;
    NTSTATUS status = CreateTopology(&topology);
    if (NT_SUCCESS(status)) {
        status = Register(Device, Irp, Resources, const_cast<PWSTR>(L"TopologyMicrophone"),
                          CLSID_PortTopology, topology, &topologyPort);
    }
    if (NT_SUCCESS(status)) {
        status = CreateWave(&wave);
    }
    if (NT_SUCCESS(status)) {
        status = Register(Device, Irp, Resources, const_cast<PWSTR>(L"WaveMicrophone"),
                          CLSID_PortWaveRT, wave, &wavePort);
    }
    if (NT_SUCCESS(status)) {
        status = PcRegisterPhysicalConnection(Device, topologyPort, kTopologyBridgePin, wavePort,
                                              kWaveBridgePin);
    }
    PUNKNOWN references[] = {topologyPort, wavePort, topology, wave};
    for (PUNKNOWN object : references) {
        if (object != nullptr) {
            object->Release();
        }
    }
    return status;
}

NTSTATUS AddDevice(_In_ PDRIVER_OBJECT Driver, _In_ PDEVICE_OBJECT PhysicalDevice) {
    PAGED_CODE();
    return PcAddAdapterDevice(Driver, PhysicalDevice, PCPFNSTARTDEVICE(StartDevice), 2, 0);
}
#pragma code_seg()

} // namespace

extern "C" DRIVER_INITIALIZE DriverEntry;

#pragma code_seg("INIT")
extern "C" NTSTATUS DriverEntry(_In_ PDRIVER_OBJECT Driver, _In_ PUNICODE_STRING RegistryPath) {
    FeedInitialize();
    return PcInitializeAdapterDriver(Driver, RegistryPath, PDRIVER_ADD_DEVICE(AddDevice));
}
#pragma code_seg()
