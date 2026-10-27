//! System FFmpeg ABI bridge, isolated from the safe RDP parser.
//! The loaded libraries outlive their objects; packets/frames are owned here.
#[cfg(not(windows))]
mod ffmpeg {
    use super::Picture;
    use libloading::Library;
    use std::{
        ffi::{c_char, c_int, c_void},
        ptr,
        sync::{Arc, OnceLock},
    };
    type P = *mut c_void;
    #[repr(C)]
    struct FramePrefix {
        data: [*const u8; 8],
        linesize: [c_int; 8],
        extended_data: P,
        width: c_int,
        height: c_int,
        samples: c_int,
        format: c_int,
    }
    pub(super) struct Api {
        find: unsafe extern "C" fn(c_int) -> P,
        context: unsafe extern "C" fn(P) -> P,
        open: unsafe extern "C" fn(P, P, *mut P) -> c_int,
        send: unsafe extern "C" fn(P, P) -> c_int,
        receive: unsafe extern "C" fn(P, P) -> c_int,
        free_context: unsafe extern "C" fn(*mut P),
        packet: unsafe extern "C" fn() -> P,
        packet_data: unsafe extern "C" fn(P, *mut u8, c_int) -> c_int,
        free_packet: unsafe extern "C" fn(*mut P),
        frame: unsafe extern "C" fn() -> P,
        free_frame: unsafe extern "C" fn(*mut P),
        malloc: unsafe extern "C" fn(usize) -> P,
        free: unsafe extern "C" fn(P),
        option: unsafe extern "C" fn(P, *const c_char, i64, c_int) -> c_int,
        scaler: unsafe extern "C" fn(
            P,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
            P,
            P,
            *const f64,
        ) -> P,
        scale: unsafe extern "C" fn(
            P,
            *const *const u8,
            *const c_int,
            c_int,
            c_int,
            *const *mut u8,
            *const c_int,
        ) -> c_int,
        free_scaler: unsafe extern "C" fn(P),
        coefficients: unsafe extern "C" fn(c_int) -> *const c_int,
        colorspace: unsafe extern "C" fn(
            P,
            *const c_int,
            c_int,
            *const c_int,
            c_int,
            c_int,
            c_int,
            c_int,
        ) -> c_int,
        _libraries: [Library; 3],
    }
    impl Api {
        fn load() -> Result<Arc<Self>, String> {
            unsafe {
                fn library(names: &[&str]) -> Result<Library, String> {
                    for name in names {
                        let directory = std::env::var_os("RUMSTSC_FFMPEG_DIR")
                            .map(std::path::PathBuf::from)
                            .or_else(|| {
                                std::env::current_exe()
                                    .ok()
                                    .and_then(|p| p.parent().map(|p| p.join("codecs")))
                                    .filter(|p| p.is_dir())
                            });
                        let path = directory.map(|p| p.join(name));
                        // Explicit directory, or the operating system library search path.
                        if let Ok(lib) = unsafe {
                            Library::new(path.as_deref().unwrap_or(std::path::Path::new(name)))
                        } {
                            return Ok(lib);
                        }
                    }
                    Err(format!(
                        "System FFmpeg unavailable ({}); install shared FFmpeg or set RUMSTSC_FFMPEG_DIR",
                        names[0]
                    ))
                }
                // Load one ABI family as a unit. Mixing libavutil and libavcodec
                // majors would make AVFrame allocation incompatible with decoding.
                let families = [
                    (
                        ["libavutil.so.58", "libavcodec.so.60", "libswscale.so.7"],
                        [58, 60, 7],
                    ),
                    (
                        ["libavutil.so.59", "libavcodec.so.61", "libswscale.so.8"],
                        [59, 61, 8],
                    ),
                    (
                        ["libavutil.so.60", "libavcodec.so.62", "libswscale.so.9"],
                        [60, 62, 9],
                    ),
                ];
                let mut compatible = None;
                for (names, majors) in families {
                    let loaded = (|| -> Result<[Library; 3], String> {
                        let libraries = [
                            library(&[names[0]])?,
                            library(&[names[1]])?,
                            library(&[names[2]])?,
                        ];
                        for (index, symbol) in [
                            b"avutil_version\0".as_slice(),
                            b"avcodec_version\0",
                            b"swscale_version\0",
                        ]
                        .iter()
                        .enumerate()
                        {
                            let version = libraries[index]
                                .get::<unsafe extern "C" fn() -> u32>(symbol)
                                .map_err(|e| e.to_string())?;
                            if version() >> 16 != majors[index] {
                                return Err("Incompatible FFmpeg library ABI".into());
                            }
                        }
                        Ok(libraries)
                    })();
                    if let Ok(libraries) = loaded {
                        compatible = Some(libraries);
                        break;
                    }
                }
                let libraries = compatible.ok_or_else(|| "Compatible shared FFmpeg 6, 7 or 8 unavailable; install a complete family or set RUMSTSC_FFMPEG_DIR".to_string())?;
                macro_rules! symbol {
                    ($i:expr,$name:literal) => {
                        *libraries[$i]
                            .get(concat!($name, "\0").as_bytes())
                            .map_err(|e| e.to_string())?
                    };
                }
                Ok(Arc::new(Self {
                    find: symbol!(1, "avcodec_find_decoder"),
                    context: symbol!(1, "avcodec_alloc_context3"),
                    open: symbol!(1, "avcodec_open2"),
                    send: symbol!(1, "avcodec_send_packet"),
                    receive: symbol!(1, "avcodec_receive_frame"),
                    free_context: symbol!(1, "avcodec_free_context"),
                    packet: symbol!(1, "av_packet_alloc"),
                    packet_data: symbol!(1, "av_packet_from_data"),
                    free_packet: symbol!(1, "av_packet_free"),
                    frame: symbol!(0, "av_frame_alloc"),
                    free_frame: symbol!(0, "av_frame_free"),
                    malloc: symbol!(0, "av_mallocz"),
                    free: symbol!(0, "av_free"),
                    option: symbol!(0, "av_opt_set_int"),
                    scaler: symbol!(2, "sws_getCachedContext"),
                    scale: symbol!(2, "sws_scale"),
                    free_scaler: symbol!(2, "sws_freeContext"),
                    coefficients: symbol!(2, "sws_getCoefficients"),
                    colorspace: symbol!(2, "sws_setColorspaceDetails"),
                    _libraries: libraries,
                }))
            }
        }
    }
    pub(super) fn api() -> Result<Arc<Api>, String> {
        static API: OnceLock<Result<Arc<Api>, String>> = OnceLock::new();
        API.get_or_init(Api::load).clone()
    }
    pub(super) struct FfmpegDecoder {
        api: Arc<Api>,
        context: P,
        frame: P,
        scaler: P,
    }
    // A decoder is confined to one graphics worker and never accessed concurrently.
    unsafe impl Send for FfmpegDecoder {}
    impl FfmpegDecoder {
        pub fn new() -> Result<Self, String> {
            let api = api()?;
            unsafe {
                let codec = (api.find)(27); // AV_CODEC_ID_H264
                if codec.is_null() {
                    return Err("System FFmpeg has no H.264 decoder".into());
                }
                let context = (api.context)(codec);
                if context.is_null() {
                    return Err("H.264 context allocation".into());
                }
                let frame = (api.frame)();
                let mut decoder = Self {
                    api,
                    context,
                    frame,
                    scaler: ptr::null_mut(),
                };
                if frame.is_null() {
                    return Err("H.264 frame allocation".into());
                }
                if (decoder.api.option)(context, c"thread_type".as_ptr(), 2, 0) < 0
                    || (decoder.api.option)(context, c"threads".as_ptr(), 2, 0) < 0
                    || (decoder.api.option)(context, c"max_pixels".as_ptr(), 4096 * 2160, 0) < 0
                {
                    return Err("H.264 decoder limits unavailable".into());
                }
                if (decoder.api.open)(context, codec, ptr::null_mut()) < 0 {
                    return Err("H.264 decoder initialization".into());
                }
                // Keep ownership established before any fallible operation above.
                decoder.scaler = ptr::null_mut();
                Ok(decoder)
            }
        }
        pub fn decode(&mut self, data: &[u8]) -> Result<Option<Picture>, String> {
            if data.is_empty() || data.len() > 16 * 1024 * 1024 {
                return Err("H.264 packet size".into());
            }
            unsafe {
                // FFmpeg requires 64 zero padding bytes beyond the packet payload.
                let memory = (self.api.malloc)(data.len() + 64).cast::<u8>();
                if memory.is_null() {
                    return Err("H.264 packet allocation".into());
                }
                ptr::copy_nonoverlapping(data.as_ptr(), memory, data.len());
                let mut packet = (self.api.packet)();
                if packet.is_null() {
                    (self.api.free)(memory.cast());
                    return Err("H.264 packet allocation".into());
                }
                if (self.api.packet_data)(packet, memory, data.len() as i32) < 0 {
                    (self.api.free)(memory.cast());
                    (self.api.free_packet)(&mut packet);
                    return Err("H.264 packet ownership".into());
                }
                let sent = (self.api.send)(self.context, packet);
                (self.api.free_packet)(&mut packet);
                if sent < 0 {
                    return Err(format!("H.264 packet rejected ({sent})"));
                }
                let status = (self.api.receive)(self.context, self.frame);
                if status == -11 {
                    return Ok(None);
                }
                if status < 0 {
                    return Err(format!("H.264 decode failed ({status})"));
                }
                // Public AVFrame prefix is stable across supported FFmpeg 6/7/8.
                let frame = &*self.frame.cast::<FramePrefix>();
                if frame.width <= 0
                    || frame.height <= 0
                    || frame.width > 4096
                    || frame.height > 2160
                {
                    return Err("H.264 picture dimensions".into());
                }
                let mut bgra = vec![0; frame.width as usize * frame.height as usize * 4];
                self.scaler = (self.api.scaler)(
                    self.scaler,
                    frame.width,
                    frame.height,
                    frame.format,
                    frame.width,
                    frame.height,
                    28,
                    2,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null(),
                );
                if self.scaler.is_null() {
                    return Err("H.264 pixel conversion context".into());
                }
                let coefficients = (self.api.coefficients)(1); // ITU-R BT.709, full range as specified by RDP AVC420.
                if (self.api.colorspace)(
                    self.scaler,
                    coefficients,
                    1,
                    coefficients,
                    1,
                    0,
                    1 << 16,
                    1 << 16,
                ) < 0
                {
                    return Err("H.264 color matrix".into());
                }
                let destination = [
                    bgra.as_mut_ptr(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                ];
                let strides = [frame.width * 4, 0, 0, 0];
                if (self.api.scale)(
                    self.scaler,
                    frame.data.as_ptr(),
                    frame.linesize.as_ptr(),
                    0,
                    frame.height,
                    destination.as_ptr(),
                    strides.as_ptr(),
                ) != frame.height
                {
                    return Err("H.264 pixel conversion".into());
                }
                Ok(Some(Picture {
                    width: frame.width as u16,
                    height: frame.height as u16,
                    bgra,
                }))
            }
        }
    }
    impl Drop for FfmpegDecoder {
        fn drop(&mut self) {
            unsafe {
                if !self.scaler.is_null() {
                    (self.api.free_scaler)(self.scaler);
                }
                (self.api.free_frame)(&mut self.frame);
                (self.api.free_context)(&mut self.context);
            }
        }
    }
}
#[cfg(not(windows))]
use ffmpeg::{FfmpegDecoder, api};
#[cfg(windows)]
use std::sync::OnceLock;
#[cfg(windows)]
mod mf;
pub fn available() -> bool {
    #[cfg(windows)]
    {
        static READY: OnceLock<bool> = OnceLock::new();
        *READY.get_or_init(|| mf::Decoder::new(64, 64).is_ok())
    }
    #[cfg(not(windows))]
    {
        api().is_ok()
    }
}
pub struct Decoder {
    #[cfg(windows)]
    inner: mf::Decoder,
    #[cfg(not(windows))]
    inner: FfmpegDecoder,
}
impl Decoder {
    pub fn new() -> Result<Self, String> {
        Self::with_size(64, 64)
    }
    pub fn with_size(width: u16, height: u16) -> Result<Self, String> {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: mf::Decoder::new(width, height)?,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (width, height);
            Ok(Self {
                inner: FfmpegDecoder::new()?,
            })
        }
    }
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<Picture>, String> {
        self.inner.decode(data)
    }
}

pub struct Picture {
    pub width: u16,
    pub height: u16,
    pub bgra: Vec<u8>,
}
