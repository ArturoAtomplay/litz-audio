// Copyright (c) Microsoft Corporation. All rights reserved.
// Derived from the Simple Audio Sample in Windows-driver-samples; see LICENSE.

#ifndef LITZ_LITZAUDIO_H
#define LITZ_LITZAUDIO_H

// portcls.h brings ks.h, which ksmedia.h requires first.
#include <portcls.h>

#include <ksmedia.h>
#include <limits.h>
#include <stdunk.h>

#include "feed.h"

#define LITZ_POOLTAG 'ztiL'

PVOID __cdecl operator new(size_t Size, POOL_FLAGS Flags, ULONG Tag);
void __cdecl operator delete(PVOID Pointer, size_t Size);

constexpr ULONG kSampleRate = 48000;
constexpr ULONG kBlockAlign = 4;
constexpr ULONG kBytesPerSecond = kSampleRate * kBlockAlign;

enum { kTopologyMicrophonePin, kTopologyBridgePin };
enum { kWaveBridgePin, kWaveCapturePin };

NTSTATUS CreateTopology(_Out_ PUNKNOWN *Unknown);
NTSTATUS CreateWave(_Out_ PUNKNOWN *Unknown);

class CStream : public IMiniportWaveRTStreamNotification,
                public IMiniportWaveRTInputStream,
                public CUnknown {
  public:
    DECLARE_STD_UNKNOWN();
    DEFINE_STD_CONSTRUCTOR(CStream);
    ~CStream();
    IMP_IMiniportWaveRTStream;
    IMP_IMiniportWaveRTStreamNotification;
    IMP_IMiniportWaveRTInputStream;

    NTSTATUS Init(_In_ PPORTWAVERTSTREAM PortStream);

  private:
    static EXT_CALLBACK Tick;
    void Advance(LONGLONG Now);
    void Arm(LONGLONG Now);
    static LONGLONG Now();
    LONGLONG Ticks(ULONGLONG Frames) const;

    static constexpr ULONG kMaxEvents = 4;

    PPORTWAVERTSTREAM m_PortStream;
    LONGLONG m_Frequency;
    PEX_TIMER m_Timer;
    KSPIN_LOCK m_Lock;
    KSSTATE m_State;
    PUCHAR m_Buffer;
    ULONG m_BufferBytes;
    ULONG m_PacketCount;
    ULONG m_PacketFrames;
    PKEVENT m_Events[kMaxEvents];
    LONGLONG m_Start;
    ULONGLONG m_Frames;
    ULONGLONG m_Packets;
    ULONG m_LastReadPacket;
};

#endif
