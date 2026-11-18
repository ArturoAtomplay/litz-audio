#include "litzaudio.h"

namespace {

constexpr ULONG kCapacity = kSampleRate / 5 * kBlockAlign;

struct Ring {
    KSPIN_LOCK lock;
    ULONG read;
    ULONG fill;
    bool running;
    UCHAR data[kCapacity];
};

Ring g_ring;

void Discard(ULONG Bytes) {
    g_ring.read = (g_ring.read + Bytes) % kCapacity;
    g_ring.fill -= Bytes;
}

} // namespace

void FeedInitialize() { KeInitializeSpinLock(&g_ring.lock); }

void FeedCaptureStart() {
    KIRQL irql;
    KeAcquireSpinLock(&g_ring.lock, &irql);
    g_ring.read = 0;
    g_ring.fill = 0;
    g_ring.running = true;
    KeReleaseSpinLock(&g_ring.lock, irql);
}

void FeedCaptureStop() {
    KIRQL irql;
    KeAcquireSpinLock(&g_ring.lock, &irql);
    g_ring.running = false;
    KeReleaseSpinLock(&g_ring.lock, irql);
}

NTSTATUS FeedWrite(_In_reads_bytes_opt_(Bytes) const UCHAR *Data, ULONG Bytes) {
    if (Data == nullptr || Bytes % kBlockAlign != 0 || Bytes > kCapacity) {
        return STATUS_INVALID_PARAMETER;
    }
    KIRQL irql;
    KeAcquireSpinLock(&g_ring.lock, &irql);
    if (g_ring.running) {
        if (g_ring.fill + Bytes > kCapacity) {
            Discard(g_ring.fill + Bytes - kCapacity);
        }
        ULONG at = (g_ring.read + g_ring.fill) % kCapacity;
        ULONG first = min(Bytes, kCapacity - at);
        RtlCopyMemory(g_ring.data + at, Data, first);
        RtlCopyMemory(g_ring.data, Data + first, Bytes - first);
        g_ring.fill += Bytes;
    }
    KeReleaseSpinLock(&g_ring.lock, irql);
    return STATUS_SUCCESS;
}

void FeedRead(_Out_writes_bytes_(Bytes) UCHAR *Data, ULONG Bytes) {
    KIRQL irql;
    KeAcquireSpinLock(&g_ring.lock, &irql);
    ULONG copied = min(Bytes, g_ring.fill);
    ULONG first = min(copied, kCapacity - g_ring.read);
    RtlCopyMemory(Data, g_ring.data + g_ring.read, first);
    RtlCopyMemory(Data + first, g_ring.data, copied - first);
    Discard(copied);
    KeReleaseSpinLock(&g_ring.lock, irql);
    RtlZeroMemory(Data + copied, Bytes - copied);
}

void FeedState(_Out_ LITZ_FEED_STATE *State) {
    KIRQL irql;
    KeAcquireSpinLock(&g_ring.lock, &irql);
    State->Running = g_ring.running;
    State->QueuedFrames = g_ring.fill / kBlockAlign;
    KeReleaseSpinLock(&g_ring.lock, irql);
}

#pragma code_seg("PAGE")
NTSTATUS PropertyHandler_Feed(_In_ PPCPROPERTY_REQUEST Request) {
    PAGED_CODE();
    if (Request->Verb & KSPROPERTY_TYPE_GET) {
        if (Request->ValueSize == 0) {
            Request->ValueSize = sizeof(LITZ_FEED_STATE);
            return STATUS_BUFFER_OVERFLOW;
        }
        if (Request->ValueSize < sizeof(LITZ_FEED_STATE)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        FeedState(static_cast<LITZ_FEED_STATE *>(Request->Value));
        Request->ValueSize = sizeof(LITZ_FEED_STATE);
        return STATUS_SUCCESS;
    }
    if (Request->Verb & KSPROPERTY_TYPE_SET) {
        return FeedWrite(static_cast<const UCHAR *>(Request->Value), Request->ValueSize);
    }
    return STATUS_INVALID_DEVICE_REQUEST;
}
#pragma code_seg()
