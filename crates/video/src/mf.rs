//! Native Windows H.264 MFT. COM objects remain on their creating thread.
use super::Picture;
use std::{marker::PhantomData, mem::ManuallyDrop, ptr, rc::Rc, sync::OnceLock};
use windows::{
    Win32::{Media::MediaFoundation::*, System::Com::*},
    core::{GUID, Interface, Result as WResult},
};
struct ComGuard(bool);
impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                CoUninitialize();
            }
        }
    }
}
pub(super) struct Decoder {
    transform: IMFTransform,
    output: Option<IMFMediaType>,
    time: i64,
    _com: ComGuard,
    _thread: PhantomData<Rc<()>>,
}
impl Decoder {
    pub fn new(width: u16, height: u16) -> Result<Self, String> {
        unsafe {
            Self::create(width, height).map_err(|e| format!("Windows H.264 initialization: {e}"))
        }
    }
    unsafe fn create(width: u16, height: u16) -> WResult<Self> {
        unsafe {
            let initialized = CoInitializeEx(None, COINIT_MULTITHREADED);
            if initialized.is_err() && initialized.0 != 0x80010106u32 as i32 {
                return Err(initialized.into());
            }
            let guard = ComGuard(initialized.is_ok());
            static START: OnceLock<std::result::Result<(), windows::core::Error>> = OnceLock::new();
            START
                .get_or_init(|| MFStartup(MF_VERSION, MFSTARTUP_LITE))
                .clone()?;
            let transform: IMFTransform = CoCreateInstance(
                &GUID::from_u128(0x62ce7e72_4c71_4d20_b15d_452831a87d9d),
                None,
                CLSCTX_INPROC_SERVER,
            )?;
            transform.GetAttributes()?.SetUINT32(&MF_LOW_LATENCY, 1)?;
            let codec: ICodecAPI = transform.cast()?;
            codec.SetValue(
                &CODECAPI_AVLowLatencyMode,
                &windows::Win32::System::Variant::VARIANT::from(1u32),
            )?;
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            input.SetUINT64(
                &MF_MT_FRAME_SIZE,
                ((width.div_ceil(16) as u64 * 16) << 32) | (height.div_ceil(16) as u64 * 16),
            )?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, 2)?;
            transform.SetInputType(0, &input, 0)?;
            let mut decoder = Self {
                transform,
                output: None,
                time: 0,
                _com: guard,
                _thread: PhantomData,
            };
            decoder.negotiate()?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(decoder)
        }
    }
    unsafe fn negotiate(&mut self) -> WResult<()> {
        unsafe {
            for index in 0..64 {
                let output = self.transform.GetOutputAvailableType(0, index)?;
                if output.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    let size = output.GetUINT64(&MF_MT_FRAME_SIZE)?;
                    if size >> 32 > 4096 || (size & 0xffffffff) > 2160 {
                        return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                            0x80070057u32 as i32,
                        )));
                    }
                    self.transform.SetOutputType(0, &output, 0)?;
                    self.output = Some(output);
                    return Ok(());
                }
            }
            Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                0x80004005u32 as i32,
            )))
        }
    }
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<Picture>, String> {
        if data.is_empty() || data.len() > 16 * 1024 * 1024 {
            return Err("H.264 packet size".into());
        }
        unsafe {
            self.decode_inner(data)
                .map_err(|e| format!("Windows H.264 decode: {e}"))
        }
    }
    unsafe fn decode_inner(&mut self, data: &[u8]) -> WResult<Option<Picture>> {
        unsafe {
            let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut pointer = ptr::null_mut();
            buffer.Lock(&mut pointer, None, None)?;
            ptr::copy_nonoverlapping(data.as_ptr(), pointer, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let input = MFCreateSample()?;
            input.AddBuffer(&buffer)?;
            input.SetSampleTime(self.time)?;
            input.SetSampleDuration(166667)?;
            self.time += 166667;
            self.transform.ProcessInput(0, &input, 0)?;
            for _ in 0..3 {
                let info = self.transform.GetOutputStreamInfo(0)?;
                let sample = if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 == 0 {
                    let sample = MFCreateSample()?;
                    let buffer = MFCreateMemoryBuffer(info.cbSize)?;
                    sample.AddBuffer(&buffer)?;
                    Some(sample)
                } else {
                    None
                };
                let mut output = [MFT_OUTPUT_DATA_BUFFER {
                    pSample: ManuallyDrop::new(sample),
                    ..Default::default()
                }];
                let mut status = 0;
                let result = self.transform.ProcessOutput(0, &mut output, &mut status);
                let sample = ManuallyDrop::take(&mut output[0].pSample);
                drop(ManuallyDrop::take(&mut output[0].pEvents));
                match result {
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        self.negotiate()?;
                        continue;
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                    Err(e) => return Err(e),
                    Ok(()) => {}
                }
                let sample = sample.ok_or_else(|| {
                    windows::core::Error::from_hresult(windows::core::HRESULT(0x80004005u32 as i32))
                })?;
                let output = self.output.as_ref().unwrap();
                let size = output.GetUINT64(&MF_MT_FRAME_SIZE)?;
                let w = (size >> 32) as usize;
                let h = (size & 0xffffffff) as usize;
                let stride = output.GetUINT32(&MF_MT_DEFAULT_STRIDE).unwrap_or(w as u32) as usize;
                if w == 0 || h == 0 || w > 4096 || h > 2160 || stride < w || stride > 8192 {
                    return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                        0x80070057u32 as i32,
                    )));
                }
                let buffer = sample.ConvertToContiguousBuffer()?;
                let mut pointer = ptr::null_mut();
                let mut length = 0;
                buffer.Lock(&mut pointer, None, Some(&mut length))?;
                if (length as usize) < stride * h + stride * h.div_ceil(2) {
                    buffer.Unlock()?;
                    return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                        0x80070057u32 as i32,
                    )));
                }
                let nv12 = std::slice::from_raw_parts(pointer, length as usize);
                let mut bgra = vec![0; w * h * 4];
                // RDP's AVC420 color space is full-range BT.709.
                for y in 0..h {
                    for x in 0..w {
                        let l = nv12[y * stride + x] as i32;
                        let chroma = stride * h + (y / 2) * stride + (x / 2) * 2;
                        let u = nv12[chroma] as i32 - 128;
                        let v = nv12[chroma + 1] as i32 - 128;
                        let p = &mut bgra[(y * w + x) * 4..][..4];
                        p[0] = (l + ((475 * u) >> 8)).clamp(0, 255) as u8;
                        p[1] = (l - ((48 * u + 120 * v) >> 8)).clamp(0, 255) as u8;
                        p[2] = (l + ((403 * v) >> 8)).clamp(0, 255) as u8;
                        p[3] = 255;
                    }
                }
                buffer.Unlock()?;
                return Ok(Some(Picture {
                    width: w as u16,
                    height: h as u16,
                    bgra,
                }));
            }
            Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                0x80004005u32 as i32,
            )))
        }
    }
}
