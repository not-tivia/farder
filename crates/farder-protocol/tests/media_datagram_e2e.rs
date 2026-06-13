//! Capstone: the inner sealed frame (farder-crypto) survives a full
//! fragment -> (server forward, simulated) -> reassemble round-trip.

use farder_crypto::media::{open_audio_wire_frame, seal_audio_packet_to_wire};
use farder_protocol::media_datagram::{fragment, OuterHeader, Reassembler, DEFAULT_MAX_DGRAM_PAYLOAD};
use farder_protocol::server::TrackKind;

#[test]
fn audio_frame_survives_fragment_reassemble_and_opens() {
    let key = [0x33u8; 32];
    let session = [0x44u8; 16];
    let speaker = [0x55u8; 32];
    let opus = vec![1u8, 2, 3, 4, 5, 6, 7, 8]; // stand-in for an Opus packet
    let sealed = seal_audio_packet_to_wire(&key, 7, &session, &speaker, &opus).unwrap();

    // Sender fragments (audio -> single datagram).
    let dgrams = fragment(TrackKind::Audio, &session, 7, &sealed, DEFAULT_MAX_DGRAM_PAYLOAD);
    assert_eq!(dgrams.len(), 1);

    // Receiver parses the outer header (as the dispatcher does), reassembles,
    // then opens the inner sealed frame.
    let mut reasm = Reassembler::new();
    let (header, payload) = OuterHeader::parse(&dgrams[0]).unwrap();
    assert_eq!(header.session_id, session);
    let reassembled = reasm.accept(&header, payload).expect("single fragment completes");
    let (seq, got_speaker, got_opus) = open_audio_wire_frame(&key, &reassembled).unwrap();
    assert_eq!(seq, 7);
    assert_eq!(got_speaker, speaker);
    assert_eq!(got_opus, opus);
}

#[test]
fn large_frame_survives_multi_fragment_reassemble() {
    // Stand-in for a big (video) sealed frame: must fragment and rejoin exactly.
    let session = [0x66u8; 16];
    let sealed: Vec<u8> = (0..5000u32).map(|i| (i * 7 + 1) as u8).collect();
    let dgrams = fragment(TrackKind::Video, &session, 42, &sealed, 1000);
    assert_eq!(dgrams.len(), 5);

    // Deliver out of order to prove ordering independence.
    let mut reasm = Reassembler::new();
    let order = [3usize, 0, 4, 1, 2];
    let mut completed = None;
    for &i in &order {
        let (header, payload) = OuterHeader::parse(&dgrams[i]).unwrap();
        if let Some(frame) = reasm.accept(&header, payload) {
            completed = Some(frame);
        }
    }
    assert_eq!(completed.as_deref(), Some(sealed.as_slice()));
}
