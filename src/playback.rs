//! Playback: fetches SoundCloud's HLS (AAC fMP4) segments over HTTP, decodes
//! them with symphonia, and plays them through rodio. Nothing is written to
//! disk. Buffers: a few HTTP segments and about 4 s of decoded audio per track.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::num::NonZero;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use reqwest::Url;
use reqwest::blocking::Client;
use rodio::{ChannelCount, DeviceSinkBuilder, SampleRate, Source};
use serde::Serialize;
use serde_json::Value;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::default::{get_codecs, get_probe};

/// Decoded packets buffered per track: about 4 s of stereo audio.
const PCM_BUFFER_PACKETS: usize = 160;
/// Samples used to fade a track out when it is skipped: about 10 ms at 44.1 kHz.
const FADE_SAMPLES: u32 = 441;
/// Segments fetched ahead of the decoder.
const SEGMENT_PREFETCH: usize = 2;
/// Tracks kept in the rodio sink at once. The next one starts before the current ends.
const SINK_DEPTH: usize = 2;

/// A queued track: display info plus the unsigned HLS transcoding URL.
#[derive(Clone)]
pub struct Track {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub transcoding: String,
    pub artwork: Option<String>,
}

/// Builds a playable track from an api-v2 track object, if it has an HLS AAC stream.
pub fn track_from_json(t: &Value) -> Option<Track> {
    Some(Track {
        id: t["id"].as_u64()?,
        title: t["title"].as_str().unwrap_or("untitled").to_string(),
        artist: t["user"]["username"].as_str().unwrap_or("unknown").to_string(),
        transcoding: pick_transcoding(t)?,
        // Large artwork is 100px; ask for the 500px variant.
        artwork: t["artwork_url"]
            .as_str()
            .map(|u| u.replace("-large.jpg", "-t500x500.jpg")),
    })
}

/// Prefers the 160 kbps AAC HLS stream, falling back to any AAC HLS stream.
fn pick_transcoding(track: &Value) -> Option<String> {
    let ts = track["media"]["transcodings"].as_array()?;
    let hls_aac = |t: &&Value| {
        t["format"]["protocol"] == "hls"
            && t["format"]["mime_type"].as_str().is_some_and(|m| m.contains("mp4a"))
    };
    ts.iter()
        .find(|t| hls_aac(t) && t["preset"] == "aac_160k")
        .or_else(|| ts.iter().find(hls_aac))
        .and_then(|t| t["url"].as_str().map(String::from))
}

#[derive(Default, Clone, Serialize)]
pub struct Status {
    pub playing: bool,
    pub paused: bool,
    pub now: Option<String>,
    pub artwork: Option<String>,
    pub queued: usize,
    pub can_prev: bool,
    pub radio: bool,
    pub error: Option<String>,
}

pub enum Msg {
    /// Append tracks to the queue.
    Queue(Vec<Track>),
    /// Stop everything and start a new queue from the top.
    Replace(Vec<Track>),
    Pause,
    Resume,
    Next,
    Prev,
    Radio(bool),
    Clear,
}

/// Handle the daemon keeps while a playback thread exists.
pub struct PlayerHandle {
    pub tx: Sender<Msg>,
    pub status: Arc<Mutex<Status>>,
}

impl PlayerHandle {
    pub fn snapshot(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn is_active(&self) -> bool {
        self.snapshot().playing
    }
}

/// Starts a playback thread. It exits by itself when the queue is empty.
pub fn spawn(creds: (String, String)) -> PlayerHandle {
    let (tx, rx) = mpsc::channel();
    let status = Arc::new(Mutex::new(Status::default()));
    let st = status.clone();
    thread::spawn(move || run(rx, st, creds));
    PlayerHandle { tx, status }
}

/// One track that has been handed to the rodio sink.
struct Slot {
    track: Track,
    cancel: Arc<AtomicBool>,
    /// Whether this track goes into history when it finishes or is skipped.
    record: bool,
}

impl Slot {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// Cancels every slot in the sink. Used for Replace and Clear.
fn cancel_all(sink_q: &mut VecDeque<Slot>) {
    for s in sink_q.iter_mut() {
        s.cancel.store(true, Ordering::Relaxed);
        s.record = false;
    }
}

/// Cancels the upcoming tracks but keeps the one playing now.
fn drop_upcoming(sink_q: &mut VecDeque<Slot>) {
    let mut seen_live = false;
    for s in sink_q.iter_mut() {
        if s.cancelled() {
            continue;
        }
        if !seen_live {
            seen_live = true;
            continue;
        }
        s.cancel.store(true, Ordering::Relaxed);
        s.record = false;
    }
}

fn live_track(sink_q: &VecDeque<Slot>) -> Option<&Track> {
    sink_q.iter().find(|s| !s.cancelled()).map(|s| &s.track)
}

fn run(rx: Receiver<Msg>, status: Arc<Mutex<Status>>, creds: (String, String)) {
    let result = (|| -> Result<()> {
        let out = DeviceSinkBuilder::open_default_sink().context("no audio output device")?;
        let player = rodio::Player::connect_new(out.mixer());
        let http = Client::new();
        let mut user_paused = false;
        let mut queue: VecDeque<Track> = VecDeque::new();
        let mut history: Vec<Track> = Vec::new();
        let mut sink_q: VecDeque<Slot> = VecDeque::new();
        let mut radio = false;
        let mut refill_after: Option<Instant> = None;

        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Msg::Queue(ts)) => queue.extend(ts),
                Ok(Msg::Replace(ts)) => {
                    cancel_all(&mut sink_q);
                    history.clear();
                    queue = ts.into();
                    radio = false;
                    refill_after = None;
                }
                Ok(Msg::Pause) => {
                    user_paused = true;
                    player.pause();
                }
                Ok(Msg::Resume) => {
                    user_paused = false;
                    player.play();
                }
                Ok(Msg::Next) => {
                    if let Some(s) = sink_q.iter().find(|s| !s.cancelled()) {
                        s.cancel.store(true, Ordering::Relaxed);
                    }
                }
                Ok(Msg::Prev) => {
                    // Replay the previous track, then the one that was playing, then the rest.
                    if let Some(prev) = history.pop() {
                        let requeue: Vec<Track> = sink_q
                            .iter()
                            .filter(|s| !s.cancelled())
                            .map(|s| s.track.clone())
                            .collect();
                        cancel_all(&mut sink_q);
                        for t in requeue.into_iter().rev() {
                            queue.push_front(t);
                        }
                        queue.push_front(prev);
                    }
                }
                Ok(Msg::Radio(on)) => {
                    radio = on;
                    if on {
                        // The current track keeps playing; the rest of the queue is replaced.
                        queue.clear();
                        drop_upcoming(&mut sink_q);
                        refill_after = None;
                    }
                }
                Ok(Msg::Clear) => {
                    queue.clear();
                    history.clear();
                    cancel_all(&mut sink_q);
                    radio = false;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }

            // Sources the sink has finished (or we cancelled) leave the list in order.
            while sink_q.len() > player.len() {
                if let Some(slot) = sink_q.pop_front() {
                    if slot.record {
                        history.push(slot.track);
                    }
                }
            }

            // Radio mode: keep the queue topped up with tracks related to the latest one.
            let due = refill_after.is_none_or(|t| Instant::now() >= t);
            if radio && queue.len() <= 2 && due {
                let seed = live_track(&sink_q).or(history.last()).map(|t| t.id);
                match seed.map(|id| fetch_related(&http, &creds, id)) {
                    Some(Ok(tracks)) => {
                        let known: Vec<u64> = history
                            .iter()
                            .map(|t| t.id)
                            .chain(sink_q.iter().map(|s| s.track.id))
                            .chain(queue.iter().map(|t| t.id))
                            .collect();
                        let fresh: Vec<Track> =
                            tracks.into_iter().filter(|t| !known.contains(&t.id)).collect();
                        // Back off when nothing new came back, so we don't hammer the API.
                        refill_after = if fresh.is_empty() {
                            Some(Instant::now() + Duration::from_secs(15))
                        } else {
                            None
                        };
                        queue.extend(fresh);
                    }
                    Some(Err(e)) => {
                        eprintln!("radio fetch failed: {e:#}");
                        refill_after = Some(Instant::now() + Duration::from_secs(15));
                    }
                    None => {}
                }
            }

            // Keep the next track decoding while the current one plays, so there is no gap.
            while player.len() < SINK_DEPTH {
                let Some(t) = queue.pop_front() else { break };
                match start_track(&http, &creds, &t, &player) {
                    Ok(cancel) => sink_q.push_back(Slot {
                        track: t,
                        cancel,
                        record: true,
                    }),
                    Err(e) => {
                        if let Ok(mut s) = status.lock() {
                            s.error = Some(format!("{}: {e:#}", t.title));
                        }
                    }
                }
            }

            // Resume after a skip if the user has not paused.
            if !user_paused && player.is_paused() {
                player.play();
            }

            if let Ok(mut s) = status.lock() {
                let live = live_track(&sink_q);
                s.playing = live.is_some() || !player.empty();
                s.paused = player.is_paused();
                s.now = live.map(|t| format!("{} — {}", t.title, t.artist));
                s.artwork = live.and_then(|t| t.artwork.clone());
                s.queued = queue.len();
                s.can_prev = !history.is_empty();
                s.radio = radio;
            }

            if queue.is_empty() && sink_q.is_empty() {
                break;
            }
        }
        Ok(())
    })();

    if let Ok(mut s) = status.lock() {
        s.playing = false;
        s.now = None;
        s.queued = 0;
        if let Err(e) = result {
            s.error = Some(format!("{e:#}"));
        }
    }
}

/// Tracks related to `id`, as the web app's radio does.
fn fetch_related(http: &Client, creds: &(String, String), id: u64) -> Result<Vec<Track>> {
    let url = Url::parse_with_params(
        &format!("https://api-v2.soundcloud.com/tracks/{id}/related"),
        &[("client_id", creds.0.as_str()), ("limit", "20")],
    )?;
    let v: Value = http
        .get(url)
        .header("Authorization", &creds.1)
        .send()?
        .error_for_status()?
        .json()?;
    Ok(v["collection"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(track_from_json)
        .collect())
}

fn start_track(
    http: &Client,
    creds: &(String, String),
    t: &Track,
    player: &rodio::Player,
) -> Result<Arc<AtomicBool>> {
    let urls = resolve_segments(http, &creds.0, &creds.1, &t.transcoding)?;
    let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(PCM_BUFFER_PACKETS);
    let (info_tx, info_rx) = mpsc::channel();
    thread::spawn(move || {
        if let Err(e) = decode_into(urls, info_tx, tx) {
            eprintln!("decode failed: {e:#}");
        }
    });
    let (rate, channels) = info_rx
        .recv()
        .context("stream ended before the first packet")?;
    let cancel = Arc::new(AtomicBool::new(false));
    player.append(PcmSource {
        rx,
        buf: Vec::new(),
        pos: 0,
        rate: NonZero::new(rate).context("invalid sample rate")?,
        channels: NonZero::new(channels).context("invalid channel count")?,
        cancel: cancel.clone(),
        fade_left: None,
    });
    Ok(cancel)
}

/// Turns the unsigned transcoding URL into the ordered list of segment URLs
/// (init segment first). The signed playlist URL expires, so resolve it per play.
fn resolve_segments(http: &Client, cid: &str, auth: &str, transcoding: &str) -> Result<Vec<String>> {
    let url = Url::parse_with_params(transcoding, &[("client_id", cid)])?;
    let v: Value = http
        .get(url)
        .header("Authorization", auth)
        .send()?
        .error_for_status()?
        .json()?;
    let playlist = v["url"].as_str().context("transcoding response has no url")?;
    let text = http.get(playlist).send()?.error_for_status()?.text()?;
    let base = Url::parse(playlist)?;

    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let uri = if let Some(rest) = line.strip_prefix("#EXT-X-MAP:URI=\"") {
            rest.split('"').next()
        } else if line.is_empty() || line.starts_with('#') {
            None
        } else {
            Some(line)
        };
        if let Some(u) = uri {
            out.push(base.join(u)?.to_string());
        }
    }
    if out.is_empty() {
        bail!("playlist has no segments");
    }
    Ok(out)
}

/// Decodes the segments in order. Sends (sample rate, channels) first, then
/// interleaved f32 packets. Returns early when the receiver is dropped.
fn decode_into(
    urls: Vec<String>,
    info: Sender<(u32, u16)>,
    tx: SyncSender<Vec<f32>>,
) -> Result<()> {
    let reader = SegmentReader {
        rx: Mutex::new(spawn_fetcher(urls)),
        cur: Vec::new(),
        pos: 0,
    };
    let mss = MediaSourceStream::new(Box::new(ReadOnlySource::new(reader)), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("m4a");
    let probed = get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())?;
    let mut format = probed.format;

    let track = format.default_track().context("no audio track")?;
    let track_id = track.id;
    let mut decoder = get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    // Channel layout is often only known after the first packet, so report it then.
    let mut info = Some(info);
    let mut channels = 0usize;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymError::DecodeError(e)) => {
                // Keep the timeline intact: replace the lost packet with silence.
                eprintln!("decode error, packet replaced with silence: {e}");
                if channels > 0 {
                    let silent = vec![0.0f32; packet.dur() as usize * channels];
                    if tx.send(silent).is_err() {
                        return Ok(());
                    }
                }
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let spec = *decoded.spec();
        channels = spec.channels.count();
        if let Some(i) = info.take() {
            let _ = i.send((spec.rate, spec.channels.count() as u16));
        }
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        if tx.send(buf.samples().to_vec()).is_err() {
            return Ok(()); // skipped or cleared
        }
    }
}

/// Downloads segments on a background thread, a few ahead of the decoder.
fn spawn_fetcher(urls: Vec<String>) -> Receiver<io::Result<Vec<u8>>> {
    let (tx, rx) = mpsc::sync_channel(SEGMENT_PREFETCH);
    thread::spawn(move || {
        let http = Client::new();
        for url in urls {
            let fetched = fetch_bytes(&http, &url);
            let failed = fetched.is_err();
            if tx.send(fetched).is_err() || failed {
                break;
            }
        }
    });
    rx
}

/// GET with a few retries, so a network blip does not end the track.
fn fetch_bytes(http: &Client, url: &str) -> io::Result<Vec<u8>> {
    let mut last = String::new();
    for attempt in 0..3u64 {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(300 * attempt));
        }
        match http
            .get(url)
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.bytes())
        {
            Ok(b) => return Ok(b.to_vec()),
            Err(e) => last = e.to_string(),
        }
    }
    Err(io::Error::other(last))
}

/// Reads the prefetched segments back to back.
struct SegmentReader {
    // Mutex makes the reader Sync, which symphonia requires; only one reader uses it.
    rx: Mutex<Receiver<io::Result<Vec<u8>>>>,
    cur: Vec<u8>,
    pos: usize,
}

impl Read for SegmentReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.cur.len() {
            let next = self
                .rx
                .lock()
                .map_err(|_| io::Error::other("segment reader poisoned"))?
                .recv();
            match next {
                Ok(Ok(bytes)) => {
                    self.cur = bytes;
                    self.pos = 0;
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => return Ok(0), // all segments delivered
            }
        }
        let n = out.len().min(self.cur.len() - self.pos);
        out[..n].copy_from_slice(&self.cur[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// rodio source fed by the decoder thread.
struct PcmSource {
    rx: Receiver<Vec<f32>>,
    buf: Vec<f32>,
    pos: usize,
    rate: SampleRate,
    channels: ChannelCount,
    cancel: Arc<AtomicBool>,
    /// Samples left in the fade-out once cancelled.
    fade_left: Option<u32>,
}

impl PcmSource {
    fn raw_next(&mut self) -> Option<f32> {
        loop {
            if self.pos < self.buf.len() {
                let s = self.buf[self.pos];
                self.pos += 1;
                return Some(s);
            }
            match self.rx.recv() {
                Ok(chunk) => {
                    self.buf = chunk;
                    self.pos = 0;
                }
                Err(_) => return None,
            }
        }
    }
}

impl Iterator for PcmSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.cancel.load(Ordering::Relaxed) {
            // Fade out so a skip does not click.
            let left = self.fade_left.get_or_insert(FADE_SAMPLES);
            if *left == 0 {
                return None;
            }
            *left -= 1;
            let gain = *left as f32 / FADE_SAMPLES as f32;
            return self.raw_next().map(|s| s * gain);
        }
        self.raw_next()
    }
}

impl Source for PcmSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        self.channels
    }

    fn sample_rate(&self) -> SampleRate {
        self.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}
