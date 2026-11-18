// Copyright (c) Microsoft Corporation. All rights reserved.
// Derived from the Simple Audio Sample in Windows-driver-samples; see LICENSE.

#include "litzaudio.h"

namespace {

// The INF names this pin GUID "Litz Microphone" under MediaCategories.
const GUID kMicrophoneName = {
    0x1dd87686, 0x96cf, 0x4275, {0x94, 0x7d, 0xbd, 0xf0, 0xbe, 0x20, 0x93, 0x4e}};

KSDATARANGE kAnalogRange = {sizeof(KSDATARANGE),
                            0,
                            0,
                            0,
                            STATICGUIDOF(KSDATAFORMAT_TYPE_AUDIO),
                            STATICGUIDOF(KSDATAFORMAT_SUBTYPE_ANALOG),
                            STATICGUIDOF(KSDATAFORMAT_SPECIFIER_NONE)};

PKSDATARANGE kAnalogRanges[] = {&kAnalogRange};

const PCPIN_DESCRIPTOR kPins[] = {
    {0,
     0,
     0,
     nullptr,
     {0, nullptr, 0, nullptr, ARRAYSIZE(kAnalogRanges), kAnalogRanges, KSPIN_DATAFLOW_IN,
      KSPIN_COMMUNICATION_NONE, &KSNODETYPE_MICROPHONE, &kMicrophoneName, 0}},
    {0,
     0,
     0,
     nullptr,
     {0, nullptr, 0, nullptr, ARRAYSIZE(kAnalogRanges), kAnalogRanges, KSPIN_DATAFLOW_OUT,
      KSPIN_COMMUNICATION_NONE, &KSCATEGORY_AUDIO, nullptr, 0}},
};

const PCCONNECTION_DESCRIPTOR kConnections[] = {
    {PCFILTER_NODE, kTopologyMicrophonePin, PCFILTER_NODE, kTopologyBridgePin},
};

const PCFILTER_DESCRIPTOR kDescriptor = {
    0, nullptr, sizeof(PCPIN_DESCRIPTOR), ARRAYSIZE(kPins), kPins, sizeof(PCNODE_DESCRIPTOR),
    0, nullptr, ARRAYSIZE(kConnections),  kConnections,     0,     nullptr};

class CTopology : public IMiniportTopology, public CUnknown {
  public:
    DECLARE_STD_UNKNOWN();
    DEFINE_STD_CONSTRUCTOR(CTopology);
    IMP_IMiniportTopology;
};

} // namespace

#pragma code_seg("PAGE")
NTSTATUS CreateTopology(_Out_ PUNKNOWN *Unknown) {
    PAGED_CODE();
    STD_CREATE_BODY_WITH_TAG_(CTopology, Unknown, nullptr, POOL_FLAG_NON_PAGED, LITZ_POOLTAG,
                              PMINIPORTTOPOLOGY);
}

STDMETHODIMP_(NTSTATUS)
CTopology::NonDelegatingQueryInterface(_In_ REFIID Interface, _COM_Outptr_ PVOID *Object) {
    PAGED_CODE();
    if (IsEqualGUIDAligned(Interface, IID_IUnknown)) {
        *Object = PVOID(PUNKNOWN(PMINIPORTTOPOLOGY(this)));
    } else if (IsEqualGUIDAligned(Interface, IID_IMiniport)) {
        *Object = PVOID(PMINIPORT(this));
    } else if (IsEqualGUIDAligned(Interface, IID_IMiniportTopology)) {
        *Object = PVOID(PMINIPORTTOPOLOGY(this));
    } else {
        *Object = nullptr;
        return STATUS_INVALID_PARAMETER;
    }
    PUNKNOWN(*Object)->AddRef();
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CTopology::Init(_In_ PUNKNOWN UnknownAdapter, _In_ PRESOURCELIST ResourceList,
                _In_ PPORTTOPOLOGY Port) {
    PAGED_CODE();
    UNREFERENCED_PARAMETER(UnknownAdapter);
    UNREFERENCED_PARAMETER(ResourceList);
    UNREFERENCED_PARAMETER(Port);
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS) CTopology::GetDescription(_Out_ PPCFILTER_DESCRIPTOR *Description) {
    PAGED_CODE();
    *Description = const_cast<PPCFILTER_DESCRIPTOR>(&kDescriptor);
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CTopology::DataRangeIntersection(_In_ ULONG PinId, _In_ PKSDATARANGE DataRange,
                                 _In_ PKSDATARANGE MatchingDataRange, _In_ ULONG OutputBufferLength,
                                 _Out_writes_bytes_to_opt_(OutputBufferLength,
                                                           *ResultantFormatLength)
                                     PVOID ResultantFormat,
                                 _Out_ PULONG ResultantFormatLength) {
    PAGED_CODE();
    UNREFERENCED_PARAMETER(PinId);
    UNREFERENCED_PARAMETER(DataRange);
    UNREFERENCED_PARAMETER(MatchingDataRange);
    UNREFERENCED_PARAMETER(OutputBufferLength);
    UNREFERENCED_PARAMETER(ResultantFormat);
    UNREFERENCED_PARAMETER(ResultantFormatLength);
    return STATUS_NOT_IMPLEMENTED;
}
#pragma code_seg()
