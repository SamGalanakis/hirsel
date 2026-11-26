# media_record

A recorder of an SFU room: a virtual down-track of the webrtc_sfu Router
(docs/spec/webrtc.md, RTC-9). It is pure and sans-IO. It takes the
plaintext media the home units already post to remote units, and writes
each recorded publication to its own file without transcoding:

| Codec | File |
| --- | --- |
| Opus | Ogg Opus (RFC 7845) |
| VP8, VP9, AV1 | WebM (Matroska), one video track |
| H.264 | fragmented MP4, an `avc3` track |

It holds no PeerSession, so it needs no ICE, DTLS or SRTP and sends no RTP.
The installed toolchain ships it as `media_record@1`, with every package it binds; a
program outside the repository binds it as `media_record = { major = 1 }`.

## 1. Use

```text
let observer = webrtc_sfu.router.new_observer(k)
let mut rec = record.recorder(record.Config(window: 64, max_wait_us: 400000, pending: 64), take observer)
// control inbox:  if let Some(ack) = record.control(mut rec, now, order) { send ack to the control unit }
// media inbox:    record.carried(mut rec, now, carried)
// every ~10 ms:   record.poll(mut rec, now)
// after each:     record.take_posts(mut rec)   -> Posts for other units
//                 record.take_output(mut rec)  -> Opened / Wrote / Closed
// at the end:     record.finish(mut rec)
```

`now` is the receive clock in ns. Files are numbered from 1. `Opened` names
the room, the publication, its owner and the format. `Wrote` carries file
bytes in order. `Closed` comes after the file's last bytes. Storing them
is the caller's job.

## 2. A unit of the router

The Observer gives the recorder its media unit index `k`. It is one more member of
the unit set that the control unit orders (RTC-5). Its seats are admitted
with home `k`. A seat subscribes like a participant (`Subscribe` with a
`Layer` target), and the subscriptions decide what is recorded: one track
and one file per down-track whose home is `k`.

The recorder keeps the accepted room state in a `webrtc_sfu.router.Observer`.
Starting with a current Observer opens files for its existing down-tracks.
After each accepted `Order`, it reads the room's down-tracks. A new
down-track opens a file, and a removed one closes it. The recorder then
answers with `Ack(room, seq, k, answer)`.

Each track posts `Wanted(mask)` for the encoding it targets to the
publication's home unit, so dynacast keeps that layer flowing (RTC-6). A
track posts mask 0 when it closes.

**Fence (RTC-7).**

- A `Media` or `Sender` message from a fenced generation, or for a
  suspended room, is dropped when it is taken (step 4).
- When a revoke removes a down-track, the track's buffered frames are
  dropped, not flushed. Its file is closed before the ack.
- The first suspension cause drops what the room's tracks buffer. The
  recorder waits for all causes to clear and then for a video key frame.
- `Unsubscribe` and `Leave` let buffered whole frames land, then close.

So no frame of a fenced owner lands in a file after the revoke's ack.

## 3. Frames and time

**Frames.** Each track keeps one `rtp_jitter` buffer for the encoding it
records. Frames come out in rtp_jitter's delivered form (rtp_jitter
README §3).

- Retarget drops old buffered frames. A `reset` on the current encoding
  finishes the old buffer and starts a new one.
- On `NeedKeyframe`, a video track posts `Key(why: Loss)` to the home unit,
  which coalesces it into a PLI or FIR upstream. Until the next key frame,
  non-key frames are dropped.

**Time.**

- Each track keeps one `rtp_jitter.sync` per encoding, fed by the
  publisher's sender reports (`Sender`, source = publication × 4 +
  encoding).
- An encoding's first frame is placed at its wall time from the report,
  less the recording's zero. The zero is the wall time of the first frame
  placed in any track.
- Later frames advance by RTP ticks from that anchor. A file's times never
  go back: a new encoding re-anchors from its own reports, but never before
  the track's last frame.
- Frames before an encoding's first report wait, up to `pending` frames.
  Past that they are dropped, and video asks for a key frame.
- A frame whose wall time is before the zero is dropped (`early`).

## 4. Files

- **Ogg Opus.** OpusHead (2 channels, pre-skip 0, 48 kHz) and OpusTags
  come first, then one packet per page. A page's granule is its packet's
  end in 48 kHz samples on the recording's timeline, so a loss leaves a
  granule gap rather than shifting later audio. The last page is marked as
  the end of the stream.
- **WebM.** The file starts at the first key frame. VP8 and VP9 take
  their size from the key frame's header. AV1 takes its `av1C` from the
  sequence header OBU. Timestamps are in ms, as SimpleBlocks in live
  (unknown-size) clusters.
- **Fragmented MP4.** The file starts at the first key frame. An `avc3`
  entry carries the first key frame's SPS and PPS in `avcC`, and parameter
  sets stay in band. One sample per fragment, on a 90 kHz timescale, with
  each sample's duration from the next frame. The last sample repeats the
  previous duration.

## 5. Limits

- VP9 SVC with several spatial layers on one SSRC is recorded as rtp_jitter
  assembles it, all layers of a timestamp in one frame. Record a single
  layer by targeting it, so that dynacast filters upstream.
- A resolution change after an encoding switch keeps the file's first
  header. WebM players and `avc3` follow the in-band change; a VP8 or VP9
  header does not record it.
- The recorder rings no doorbell, so its latency is the media inbox's.
- The cross-unit contract (posts, inbox messages) follows `webrtc_sfu.units`
  by reading its code. No law runs the recorder inside a units hall.

## 6. Laws

`laws.hirsel` drives the recorder through `drive.hirsel`'s rig with real
FFmpeg 6.1.1 frames (`frames.hirsel`, written by `vectors/gen_frames.py`):
two seconds of VP8, VP9, AV1 and H.264 at 30 fps (key frames 0 and 30),
and 20 ms Opus packets. Each codec is packetized by its own RTP payload
format. Expected values come from the plan: the frames sent, their
capture times and the sender reports.

| Law | Bug it catches |
| --- | --- |
| `recording_keeps_every_frame_on_the_sender_clock` | timestamps taken in 32 bits across the RTP wrap; frames behind a hole not flushed at the end; frames or key flags changed |
| `audio_and_video_line_up_by_sender_reports` | each file timed from its own first frame (A/V offset ≥ 40 ms) |
| `loss_asks_a_key_frame_and_skips_to_it` | non-key frames written after a loss; no key request |
| `revoke_fences_buffered_media` | a revoked owner's buffered frames flushed before the ack, or the file changed after it |
| `suspend_drops_buffered_media` | a suspension that keeps buffered frames, so a key frame taken before it lands after it ends |

`test-vectors.sh WORK` records each video codec with Opus, then has
ffprobe and ffmpeg (`vectors/validate.py`) open and decode every file with
no error. It checks one decoded frame per packet, rising timestamps within
one frame of the true times, each file's span within one frame, and an
A/V offset under 40 ms.
