#ifndef LITZ_FEED_H
#define LITZ_FEED_H

#include <portcls.h>

// Wire contract with litzd (crates/desktop/src/driver.rs). SET appends
// 32-bit mono PCM at 48 kHz; GET returns LITZ_FEED_STATE.
#define STATIC_KSPROPSETID_LitzFeed                                                                \
    0x89b4253b, 0x9034, 0x497d, 0x98, 0x78, 0xae, 0xd9, 0x54, 0xe0, 0x1f, 0xc6
DEFINE_GUIDSTRUCT("89B4253B-9034-497D-9878-AED954E01FC6", KSPROPSETID_LitzFeed);
#define KSPROPSETID_LitzFeed DEFINE_GUIDNAMED(KSPROPSETID_LitzFeed)
#define KSPROPERTY_LITZ_FEED 0

typedef struct {
    ULONG Running;
    ULONG QueuedFrames;
} LITZ_FEED_STATE;

void FeedInitialize();
void FeedCaptureStart();
void FeedCaptureStop();
NTSTATUS FeedWrite(_In_reads_bytes_opt_(Bytes) const UCHAR *Data, ULONG Bytes);
void FeedRead(_Out_writes_bytes_(Bytes) UCHAR *Data, ULONG Bytes);
void FeedState(_Out_ LITZ_FEED_STATE *State);
NTSTATUS PropertyHandler_Feed(_In_ PPCPROPERTY_REQUEST Request);

#endif
