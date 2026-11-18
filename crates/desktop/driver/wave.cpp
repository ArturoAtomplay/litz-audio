// Copyright (c) Microsoft Corporation. All rights reserved.
// Derived from the Simple Audio Sample in Windows-driver-samples; see LICENSE.

#include "litzaudio.h"

namespace {

const KSDATAFORMAT_WAVEFORMATEXTENSIBLE kFormat = {
    {sizeof(KSDATAFORMAT_WAVEFORMATEXTENSIBLE), 0, 0, 0, STATICGUIDOF(KSDATAFORMAT_TYPE_AUDIO),
     STATICGUIDOF(KSDATAFORMAT_SUBTYPE_PCM), STATICGUIDOF(KSDATAFORMAT_SPECIFIER_WAVEFORMATEX)},
    {{WAVE_FORMAT_EXTENSIBLE, 1, kSampleRate, kBytesPerSecond, kBlockAlign, 32,
      sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX)},
     {32},
     KSAUDIO_SPEAKER_MONO,
     STATICGUIDOF(KSDATAFORMAT_SUBTYPE_PCM)}};

KSDATARANGE_AUDIO kCaptureRange = {
    {sizeof(KSDATARANGE_AUDIO), 0, 0, 0, STATICGUIDOF(KSDATAFORMAT_TYPE_AUDIO),
     STATICGUIDOF(KSDATAFORMAT_SUBTYPE_PCM), STATICGUIDOF(KSDATAFORMAT_SPECIFIER_WAVEFORMATEX)},
    1,
    32,
    32,
    kSampleRate,
    kSampleRate};

KSDATARANGE kBridgeRange = {sizeof(KSDATARANGE),
                            0,
                            0,
                            0,
                            STATICGUIDOF(KSDATAFORMAT_TYPE_AUDIO),
                            STATICGUIDOF(KSDATAFORMAT_SUBTYPE_ANALOG),
                            STATICGUIDOF(KSDATAFORMAT_SPECIFIER_NONE)};

PKSDATARANGE kCaptureRanges[] = {PKSDATARANGE(&kCaptureRange)};
PKSDATARANGE kBridgeRanges[] = {&kBridgeRange};

const PCPIN_DESCRIPTOR kPins[] = {
    {0,
     0,
     0,
     nullptr,
     {0, nullptr, 0, nullptr, ARRAYSIZE(kBridgeRanges), kBridgeRanges, KSPIN_DATAFLOW_IN,
      KSPIN_COMMUNICATION_NONE, &KSCATEGORY_AUDIO, nullptr, 0}},
    {1,
     1,
     0,
     nullptr,
     {0, nullptr, 0, nullptr, ARRAYSIZE(kCaptureRanges), kCaptureRanges, KSPIN_DATAFLOW_OUT,
      KSPIN_COMMUNICATION_SINK, &KSCATEGORY_AUDIO, &KSAUDFNAME_RECORDING_CONTROL, 0}},
};

const PCNODE_DESCRIPTOR kNodes[] = {{0, nullptr, &KSNODETYPE_ADC, nullptr}};

const PCCONNECTION_DESCRIPTOR kConnections[] = {
    {PCFILTER_NODE, kWaveBridgePin, 0, 1},
    {0, 0, PCFILTER_NODE, kWaveCapturePin},
};

const PCPROPERTY_ITEM kProperties[] = {
    {&KSPROPSETID_LitzFeed, KSPROPERTY_LITZ_FEED, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET,
     PropertyHandler_Feed},
};

DEFINE_PCAUTOMATION_TABLE_PROP(kAutomation, kProperties);

const PCFILTER_DESCRIPTOR kDescriptor = {0,
                                         &kAutomation,
                                         sizeof(PCPIN_DESCRIPTOR),
                                         ARRAYSIZE(kPins),
                                         kPins,
                                         sizeof(PCNODE_DESCRIPTOR),
                                         ARRAYSIZE(kNodes),
                                         kNodes,
                                         ARRAYSIZE(kConnections),
                                         kConnections,
                                         0,
                                         nullptr};

bool IsOurFormat(_In_ PKSDATAFORMAT Format) {
    if (Format->FormatSize < sizeof(KSDATAFORMAT_WAVEFORMATEX) ||
        !IsEqualGUIDAligned(Format->MajorFormat, KSDATAFORMAT_TYPE_AUDIO) ||
        !IsEqualGUIDAligned(Format->SubFormat, KSDATAFORMAT_SUBTYPE_PCM) ||
        !IsEqualGUIDAligned(Format->Specifier, KSDATAFORMAT_SPECIFIER_WAVEFORMATEX)) {
        return false;
    }
    auto wave = reinterpret_cast<PWAVEFORMATEX>(Format + 1);
    if (wave->nChannels != 1 || wave->nSamplesPerSec != kSampleRate ||
        wave->nBlockAlign != kBlockAlign || wave->wBitsPerSample != 32) {
        return false;
    }
    if (wave->wFormatTag == WAVE_FORMAT_PCM) {
        return true;
    }
    if (wave->wFormatTag != WAVE_FORMAT_EXTENSIBLE ||
        Format->FormatSize < sizeof(KSDATAFORMAT_WAVEFORMATEXTENSIBLE) ||
        wave->cbSize < sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX)) {
        return false;
    }
    auto extensible = reinterpret_cast<PWAVEFORMATEXTENSIBLE>(wave);
    return extensible->Samples.wValidBitsPerSample == 32 &&
           IsEqualGUIDAligned(extensible->SubFormat, KSDATAFORMAT_SUBTYPE_PCM);
}

class CWave : public IMiniportWaveRT, public CUnknown {
  public:
    DECLARE_STD_UNKNOWN();
    DEFINE_STD_CONSTRUCTOR(CWave);
    IMP_IMiniportWaveRT;
};

} // namespace

#pragma code_seg("PAGE")
NTSTATUS CreateWave(_Out_ PUNKNOWN *Unknown) {
    PAGED_CODE();
    STD_CREATE_BODY_WITH_TAG_(CWave, Unknown, nullptr, POOL_FLAG_NON_PAGED, LITZ_POOLTAG,
                              PMINIPORTWAVERT);
}

STDMETHODIMP_(NTSTATUS)
CWave::NonDelegatingQueryInterface(_In_ REFIID Interface, _COM_Outptr_ PVOID *Object) {
    PAGED_CODE();
    if (IsEqualGUIDAligned(Interface, IID_IUnknown)) {
        *Object = PVOID(PUNKNOWN(PMINIPORTWAVERT(this)));
    } else if (IsEqualGUIDAligned(Interface, IID_IMiniport)) {
        *Object = PVOID(PMINIPORT(this));
    } else if (IsEqualGUIDAligned(Interface, IID_IMiniportWaveRT)) {
        *Object = PVOID(PMINIPORTWAVERT(this));
    } else {
        *Object = nullptr;
        return STATUS_INVALID_PARAMETER;
    }
    PUNKNOWN(*Object)->AddRef();
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CWave::Init(_In_ PUNKNOWN UnknownAdapter, _In_ PRESOURCELIST ResourceList, _In_ PPORTWAVERT Port) {
    PAGED_CODE();
    UNREFERENCED_PARAMETER(UnknownAdapter);
    UNREFERENCED_PARAMETER(ResourceList);
    UNREFERENCED_PARAMETER(Port);
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS) CWave::GetDescription(_Out_ PPCFILTER_DESCRIPTOR *Description) {
    PAGED_CODE();
    *Description = const_cast<PPCFILTER_DESCRIPTOR>(&kDescriptor);
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CWave::DataRangeIntersection(_In_ ULONG PinId, _In_ PKSDATARANGE DataRange,
                             _In_ PKSDATARANGE MatchingDataRange, _In_ ULONG OutputBufferLength,
                             _Out_writes_bytes_to_opt_(OutputBufferLength, *ResultantFormatLength)
                                 PVOID ResultantFormat,
                             _Out_ PULONG ResultantFormatLength) {
    PAGED_CODE();
    UNREFERENCED_PARAMETER(MatchingDataRange);
    if (PinId != kWaveCapturePin ||
        !IsEqualGUIDAligned(DataRange->Specifier, KSDATAFORMAT_SPECIFIER_WAVEFORMATEX)) {
        return STATUS_NOT_IMPLEMENTED;
    }
    if (DataRange->FormatSize >= sizeof(KSDATARANGE_AUDIO)) {
        auto audio = reinterpret_cast<PKSDATARANGE_AUDIO>(DataRange);
        if (audio->MaximumChannels < 1 || audio->MinimumBitsPerSample > 32 ||
            audio->MaximumBitsPerSample < 32 || audio->MinimumSampleFrequency > kSampleRate ||
            audio->MaximumSampleFrequency < kSampleRate) {
            return STATUS_NO_MATCH;
        }
    }
    *ResultantFormatLength = sizeof(kFormat);
    if (OutputBufferLength == 0) {
        return STATUS_BUFFER_OVERFLOW;
    }
    if (OutputBufferLength < sizeof(kFormat)) {
        return STATUS_BUFFER_TOO_SMALL;
    }
    *static_cast<PKSDATAFORMAT_WAVEFORMATEXTENSIBLE>(ResultantFormat) = kFormat;
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CWave::NewStream(_Out_ PMINIPORTWAVERTSTREAM *Stream, _In_ PPORTWAVERTSTREAM PortStream,
                 _In_ ULONG Pin, _In_ BOOLEAN Capture, _In_ PKSDATAFORMAT DataFormat) {
    PAGED_CODE();
    *Stream = nullptr;
    if (Pin != kWaveCapturePin || !Capture || !IsOurFormat(DataFormat)) {
        return STATUS_NO_MATCH;
    }
    auto stream = new (POOL_FLAG_NON_PAGED, LITZ_POOLTAG) CStream(nullptr);
    if (stream == nullptr) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    stream->AddRef();
    NTSTATUS status = stream->Init(PortStream);
    if (NT_SUCCESS(status)) {
        *Stream = PMINIPORTWAVERTSTREAM(PMINIPORTWAVERTSTREAMNOTIFICATION(stream));
        (*Stream)->AddRef();
    }
    stream->Release();
    return status;
}

STDMETHODIMP_(NTSTATUS) CWave::GetDeviceDescription(_Out_ PDEVICE_DESCRIPTION Description) {
    PAGED_CODE();
    RtlZeroMemory(Description, sizeof(*Description));
    Description->Master = TRUE;
    Description->ScatterGather = TRUE;
    Description->Dma32BitAddresses = TRUE;
    Description->InterfaceType = PCIBus;
    Description->MaximumLength = 0xFFFFFFFF;
    return STATUS_SUCCESS;
}
#pragma code_seg()
