use sce_core::analyzer::{analyze_file, AnalyzerError};
use std::path::PathBuf;

#[tokio::test]
async fn mp3_fixture_decodes_and_analyzes() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.mp3");
    let metrics = analyze_file(&fixture_path, "TelemetryAnalyzer/1.0.0")
        .await
        .expect("analyze tiny mp3 fixture");

    assert!(metrics.lossy_source);
    assert!(metrics.frame_count > 0);
    assert!(metrics.sample_rate_hz > 0);
    assert_eq!(metrics.tonal_balance_curve.len(), 30);
    assert!(metrics.crest_factor_db >= 0.0);
}

#[tokio::test]
async fn corrupt_file_returns_decode_error() {
    let td = tempfile::tempdir().expect("tempdir");
    let corrupt_path = td.path().join("corrupt.mp3");
    tokio::fs::write(&corrupt_path, b"not-an-mp3")
        .await
        .expect("write corrupt file");

    let err = analyze_file(&corrupt_path, "TelemetryAnalyzer/1.0.0")
        .await
        .expect_err("analyzer should fail on corrupt file");

    match err {
        AnalyzerError::Decode(reason) => assert!(!reason.trim().is_empty()),
        other => panic!("expected decode error, got {other:?}"),
    }
}
