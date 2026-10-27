use rumstsc_video::{Decoder, available};
#[test]
fn real_h264_intra_and_predictive_packets() {
    #[cfg(windows)]
    let _native = Decoder::new().expect("Windows native H.264 must initialize");
    if !available() {
        eprintln!("No system FFmpeg; native video test skipped");
        return;
    }
    let mut decoder = Decoder::new().unwrap();
    let first = decoder
        .decode(include_bytes!("data/gray-idr.h264"))
        .unwrap()
        .expect("first IDR must have no extra frame latency");
    assert_eq!((first.width, first.height), (64, 64));
    assert!(first.bgra.as_chunks::<4>().0.iter().all(|p| p[3] == 255
        && p[0].abs_diff(100) <= 2
        && p[1].abs_diff(100) <= 2
        && p[2].abs_diff(100) <= 2));
    let second = decoder
        .decode(include_bytes!("data/gray-p.h264"))
        .unwrap()
        .expect("P frame must use retained references");
    assert_eq!(first.bgra, second.bgra);
    assert!(decoder.decode(&[]).is_err());
}

#[test]
fn avc420_full_range_bt709_color() {
    if !available() {
        return;
    }
    let frame = Decoder::new()
        .unwrap()
        .decode(include_bytes!("data/color-idr.h264"))
        .unwrap()
        .unwrap();
    assert!(
        frame
            .bgra
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[0].abs_diff(29) <= 2
                && p[1].abs_diff(83) <= 2
                && p[2].abs_diff(182) <= 2
                && p[3] == 255),
        "decoded color {:?}",
        &frame.bgra[..4]
    );
}
