// Copyright (c) Microsoft Corporation. All rights reserved.
// Derived from the Simple Audio Sample in Windows-driver-samples; see LICENSE.

#include "litzaudio.h"

// Paged code runs at PASSIVE_LEVEL only and never takes m_Lock: Driver
// Verifier pages it out, and a spin lock raises IRQL to DISPATCH_LEVEL.
#pragma code_seg("PAGE")
NTSTATUS CStream::Init(_In_ PPORTWAVERTSTREAM PortStream) {
    PAGED_CODE();
    m_PortStream = PortStream;
    m_State = KSSTATE_STOP;
    m_LastReadPacket = ULONG_MAX;
    KeQueryPerformanceCounter(reinterpret_cast<PLARGE_INTEGER>(&m_Frequency));
    KeInitializeSpinLock(&m_Lock);
    m_Timer = ExAllocateTimer(Tick, this, EX_TIMER_HIGH_RESOLUTION);
    return m_Timer ? STATUS_SUCCESS : STATUS_INSUFFICIENT_RESOURCES;
}

CStream::~CStream() {
    PAGED_CODE();
    if (m_Timer) {
        ExDeleteTimer(m_Timer, TRUE, TRUE, nullptr);
    }
}

STDMETHODIMP_(NTSTATUS)
CStream::NonDelegatingQueryInterface(_In_ REFIID Interface, _COM_Outptr_ PVOID *Object) {
    PAGED_CODE();
    if (IsEqualGUIDAligned(Interface, IID_IUnknown)) {
        *Object = PVOID(PUNKNOWN(PMINIPORTWAVERTSTREAMNOTIFICATION(this)));
    } else if (IsEqualGUIDAligned(Interface, IID_IMiniportWaveRTStream)) {
        *Object = PVOID(PMINIPORTWAVERTSTREAM(PMINIPORTWAVERTSTREAMNOTIFICATION(this)));
    } else if (IsEqualGUIDAligned(Interface, IID_IMiniportWaveRTStreamNotification)) {
        *Object = PVOID(PMINIPORTWAVERTSTREAMNOTIFICATION(this));
    } else if (IsEqualGUIDAligned(Interface, IID_IMiniportWaveRTInputStream)) {
        *Object = PVOID(PMINIPORTWAVERTINPUTSTREAM(this));
    } else {
        *Object = nullptr;
        return STATUS_INVALID_PARAMETER;
    }
    PUNKNOWN(*Object)->AddRef();
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CStream::AllocateBufferWithNotification(_In_ ULONG NotificationCount, _In_ ULONG RequestedSize,
                                        _Out_ PMDL *AudioBufferMdl, _Out_ ULONG *ActualSize,
                                        _Out_ ULONG *OffsetFromFirstPage,
                                        _Out_ MEMORY_CACHING_TYPE *CacheType) {
    PAGED_CODE();
    if (NotificationCount == 0 || RequestedSize % NotificationCount != 0 ||
        RequestedSize / NotificationCount % kBlockAlign != 0) {
        return STATUS_INVALID_PARAMETER;
    }
    NTSTATUS status = AllocateAudioBuffer(RequestedSize, AudioBufferMdl, ActualSize,
                                          OffsetFromFirstPage, CacheType);
    if (NT_SUCCESS(status)) {
        m_PacketCount = NotificationCount;
        m_PacketFrames = RequestedSize / NotificationCount / kBlockAlign;
    }
    return status;
}

STDMETHODIMP_(NTSTATUS)
CStream::AllocateAudioBuffer(_In_ ULONG RequestedSize, _Out_ PMDL *AudioBufferMdl,
                             _Out_ ULONG *ActualSize, _Out_ ULONG *OffsetFromFirstPage,
                             _Out_ MEMORY_CACHING_TYPE *CacheType) {
    PAGED_CODE();
    RequestedSize -= RequestedSize % kBlockAlign;
    if (RequestedSize == 0) {
        return STATUS_INVALID_PARAMETER;
    }
    PHYSICAL_ADDRESS highest;
    highest.QuadPart = MAXULONG;
    PMDL mdl = m_PortStream->AllocatePagesForMdl(highest, RequestedSize);
    if (mdl == nullptr) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    m_Buffer = static_cast<PUCHAR>(m_PortStream->MapAllocatedPages(mdl, MmCached));
    if (m_Buffer == nullptr) {
        m_PortStream->FreePagesFromMdl(mdl);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    m_BufferBytes = RequestedSize;
    m_PacketCount = 0;
    *AudioBufferMdl = mdl;
    *ActualSize = RequestedSize;
    *OffsetFromFirstPage = 0;
    *CacheType = MmCached;
    return STATUS_SUCCESS;
}

STDMETHODIMP_(VOID) CStream::FreeAudioBuffer(_In_opt_ PMDL Mdl, _In_ ULONG BufferSize) {
    PAGED_CODE();
    UNREFERENCED_PARAMETER(BufferSize);
    if (Mdl != nullptr) {
        if (m_Buffer != nullptr) {
            m_PortStream->UnmapAllocatedPages(m_Buffer, Mdl);
        }
        m_PortStream->FreePagesFromMdl(Mdl);
    }
    m_Buffer = nullptr;
    m_BufferBytes = 0;
    m_PacketCount = 0;
}

STDMETHODIMP_(VOID) CStream::FreeBufferWithNotification(_In_ PMDL Mdl, _In_ ULONG BufferSize) {
    PAGED_CODE();
    FreeAudioBuffer(Mdl, BufferSize);
}

STDMETHODIMP_(NTSTATUS) CStream::SetFormat(_In_ PKSDATAFORMAT DataFormat) {
    PAGED_CODE();
    UNREFERENCED_PARAMETER(DataFormat);
    return STATUS_NOT_SUPPORTED;
}

STDMETHODIMP_(VOID) CStream::GetHWLatency(_Out_ KSRTAUDIO_HWLATENCY *Latency) {
    PAGED_CODE();
    RtlZeroMemory(Latency, sizeof(*Latency));
}

STDMETHODIMP_(NTSTATUS) CStream::GetPositionRegister(_Out_ KSRTAUDIO_HWREGISTER *Register) {
    PAGED_CODE();
    UNREFERENCED_PARAMETER(Register);
    return STATUS_NOT_IMPLEMENTED;
}

STDMETHODIMP_(NTSTATUS) CStream::GetClockRegister(_Out_ KSRTAUDIO_HWREGISTER *Register) {
    PAGED_CODE();
    UNREFERENCED_PARAMETER(Register);
    return STATUS_NOT_IMPLEMENTED;
}
#pragma code_seg()

STDMETHODIMP_(NTSTATUS) CStream::RegisterNotificationEvent(_In_ PKEVENT NotificationEvent) {
    NTSTATUS status = STATUS_INSUFFICIENT_RESOURCES;
    KIRQL irql;
    KeAcquireSpinLock(&m_Lock, &irql);
    for (PKEVENT &slot : m_Events) {
        if (slot == NotificationEvent) {
            status = STATUS_UNSUCCESSFUL;
            break;
        }
    }
    if (status == STATUS_INSUFFICIENT_RESOURCES) {
        for (PKEVENT &slot : m_Events) {
            if (slot == nullptr) {
                slot = NotificationEvent;
                status = STATUS_SUCCESS;
                break;
            }
        }
    }
    KeReleaseSpinLock(&m_Lock, irql);
    return status;
}

STDMETHODIMP_(NTSTATUS) CStream::UnregisterNotificationEvent(_In_ PKEVENT NotificationEvent) {
    NTSTATUS status = STATUS_NOT_FOUND;
    KIRQL irql;
    KeAcquireSpinLock(&m_Lock, &irql);
    for (PKEVENT &slot : m_Events) {
        if (slot == NotificationEvent) {
            slot = nullptr;
            status = STATUS_SUCCESS;
            break;
        }
    }
    KeReleaseSpinLock(&m_Lock, irql);
    return status;
}

LONGLONG CStream::Now() { return KeQueryPerformanceCounter(nullptr).QuadPart; }

LONGLONG CStream::Ticks(ULONGLONG Frames) const {
    return static_cast<LONGLONG>(Frames / kSampleRate) * m_Frequency +
           static_cast<LONGLONG>(Frames % kSampleRate) * m_Frequency / kSampleRate;
}

// Fills the buffer up to the frame due at `Now` from the feed. Caller holds
// m_Lock; the feed's lock nests inside it.
void CStream::Advance(LONGLONG Now) {
    if (m_State != KSSTATE_RUN || m_BufferBytes == 0) {
        return;
    }
    ULONGLONG elapsed = static_cast<ULONGLONG>(max(Now - m_Start, 0LL));
    ULONGLONG frequency = static_cast<ULONGLONG>(m_Frequency);
    ULONGLONG due =
        elapsed / frequency * kSampleRate + elapsed % frequency * kSampleRate / frequency;
    if (due <= m_Frames) {
        return;
    }
    ULONGLONG frames = min(due - m_Frames, static_cast<ULONGLONG>(m_BufferBytes / kBlockAlign));
    ULONG offset = static_cast<ULONG>((due - frames) * kBlockAlign % m_BufferBytes);
    ULONG bytes = static_cast<ULONG>(frames * kBlockAlign);
    while (bytes > 0) {
        ULONG run = min(bytes, m_BufferBytes - offset);
        FeedRead(m_Buffer + offset, run);
        offset = (offset + run) % m_BufferBytes;
        bytes -= run;
    }
    m_Frames = due;
    if (m_PacketCount != 0) {
        m_Packets = m_Frames / m_PacketFrames;
    }
}

// Schedules the next tick at the end of the packet now being filled.
void CStream::Arm(LONGLONG Now) {
    LONGLONG due = m_Start + Ticks((m_Frames / m_PacketFrames + 1) * m_PacketFrames) - Now;
    LONGLONG hns =
        max(due / m_Frequency * 10000000 + due % m_Frequency * 10000000 / m_Frequency, 1LL);
    ExSetTimer(m_Timer, -hns, 0, nullptr);
}

void CStream::Tick(_In_ PEX_TIMER Timer, _In_opt_ PVOID Context) {
    UNREFERENCED_PARAMETER(Timer);
    auto stream = static_cast<CStream *>(Context);
    if (stream == nullptr) {
        return;
    }
    KIRQL irql;
    KeAcquireSpinLock(&stream->m_Lock, &irql);
    if (stream->m_State == KSSTATE_RUN && stream->m_PacketCount != 0) {
        LONGLONG now = Now();
        ULONGLONG before = stream->m_Packets;
        stream->Advance(now);
        if (stream->m_Packets != before) {
            for (PKEVENT event : stream->m_Events) {
                if (event != nullptr) {
                    KeSetEvent(event, 0, FALSE);
                }
            }
        }
        stream->Arm(now);
    }
    KeReleaseSpinLock(&stream->m_Lock, irql);
}

STDMETHODIMP_(NTSTATUS) CStream::SetState(_In_ KSSTATE State) {
    KIRQL irql;
    if (State == KSSTATE_RUN) {
        FeedCaptureStart();
    }
    KeAcquireSpinLock(&m_Lock, &irql);
    LONGLONG now = Now();
    switch (State) {
    case KSSTATE_STOP:
        m_Frames = 0;
        m_Packets = 0;
        m_LastReadPacket = ULONG_MAX;
        break;
    case KSSTATE_PAUSE:
        Advance(now);
        break;
    case KSSTATE_RUN:
        m_Start = now - Ticks(m_Frames);
        break;
    default:
        break;
    }
    m_State = State;
    if (State == KSSTATE_RUN && m_PacketCount != 0) {
        Arm(now);
    }
    KeReleaseSpinLock(&m_Lock, irql);
    if (State != KSSTATE_RUN) {
        ExCancelTimer(m_Timer, nullptr);
        FeedCaptureStop();
    }
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS) CStream::GetPosition(_Out_ PKSAUDIO_POSITION Position) {
    KIRQL irql;
    KeAcquireSpinLock(&m_Lock, &irql);
    Advance(Now());
    ULONGLONG offset = m_BufferBytes ? m_Frames * kBlockAlign % m_BufferBytes : 0;
    Position->PlayOffset = offset;
    Position->WriteOffset = offset;
    KeReleaseSpinLock(&m_Lock, irql);
    return STATUS_SUCCESS;
}

// Reports the newest completed packet. With the engine's two packets per
// buffer, an older one is already being overwritten, so a late reader skips.
NTSTATUS CStream::GetReadPacket(_Out_ ULONG *PacketNumber, _Out_ DWORD *Flags,
                                _Out_ ULONG64 *PerformanceCounterValue, _Out_ BOOL *MoreData) {
    *Flags = 0;
    *MoreData = FALSE;
    if (m_PacketCount == 0) {
        return STATUS_NOT_SUPPORTED;
    }
    KIRQL irql;
    KeAcquireSpinLock(&m_Lock, &irql);
    if (m_State < KSSTATE_PAUSE) {
        KeReleaseSpinLock(&m_Lock, irql);
        return STATUS_INVALID_DEVICE_STATE;
    }
    ULONGLONG packets = m_Packets;
    ULONG available = static_cast<ULONG>(packets - 1);
    if (packets == 0 || available == m_LastReadPacket) {
        KeReleaseSpinLock(&m_Lock, irql);
        return STATUS_DEVICE_NOT_READY;
    }
    m_LastReadPacket = available;
    *PacketNumber = available;
    *PerformanceCounterValue =
        static_cast<ULONG64>(m_Start + Ticks((packets - 1) * m_PacketFrames));
    KeReleaseSpinLock(&m_Lock, irql);
    return STATUS_SUCCESS;
}
