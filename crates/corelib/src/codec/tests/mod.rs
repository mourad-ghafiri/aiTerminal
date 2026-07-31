use super::*;

#[test]
fn base64_round_trips_and_matches_vectors() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64_decode("Zm9vYmFy").unwrap(), b"foobar");
    assert_eq!(base64_decode("Zg==").unwrap(), b"f");
    assert!(base64_decode("###").is_err());
}

#[test]
fn hex_and_url_round_trip() {
    assert_eq!(hex_encode(b"\x00\xff\x10"), "00ff10");
    assert_eq!(hex_decode("00ff10").unwrap(), b"\x00\xff\x10");
    assert_eq!(url_encode("a b/c?=&"), "a%20b%2Fc%3F%3D%26");
    assert_eq!(url_decode("a%20b%2Fc").unwrap(), "a b/c");
}

#[test]
fn sha256_matches_known_vectors() {
    assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    // a >64-byte message exercises multi-block padding
    assert_eq!(
        sha256_hex(b"The quick brown fox jumps over the lazy dog"),
        "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592"
    );
}

#[test]
fn csv_round_trips_quotes_and_commas() {
    let rows = csv_parse("a,b,c\n\"x,y\",\"he said \"\"hi\"\"\",z\n");
    assert_eq!(rows[0], vec!["a", "b", "c"]);
    assert_eq!(rows[1], vec!["x,y", "he said \"hi\"", "z"]);
    let back = csv_format(&rows);
    assert_eq!(csv_parse(&back), rows);
}
