//! Proof that CNG and `horadric-link` speak the same bytes: a PC (CNG)
//! and a Linux node (`horadric-link`) must agree on keys, codes and
//! sealed frames, or a node could never join. Each test runs one side on
//! each implementation, both ways round, so neither can drift alone.

use horadric_link::ecdh;

use crate::joining::{agree, frame_aad, session_key, Way};
use crate::portal_crypto as cng;
use crate::portal_push::{ecdh as cng_ecdh, ecdh_pair as cng_pair};

/// `04 || x || y` of a key CNG made, read from its `x || y || d`.
fn cng_public(private: &[u8; 96]) -> Vec<u8> {
    let mut public = vec![4u8];
    public.extend_from_slice(&private[..64]);
    public
}

#[test]
fn each_reads_the_others_stored_keys() {
    let windows = cng_pair().unwrap();
    // The link derives the same public key from CNG's private one, so it
    // can load a `brothers.json` identity made on Windows.
    assert_eq!(ecdh::public(&windows).to_vec(), cng_public(&windows));
    let d: [u8; 32] = windows[64..].try_into().unwrap();
    assert_eq!(ecdh::from_scalar(&d), Some(windows));
    // And CNG imports a pair the link made, x || y || d as it keeps them.
    let linux = ecdh::pair();
    let other = cng_pair().unwrap();
    assert_eq!(
        cng_ecdh(&linux, &cng_public(&other)).unwrap(),
        cng_ecdh(&other, &ecdh::public(&linux)).unwrap()
    );
}

#[test]
fn agreement_gives_the_same_secret_both_ways() {
    for _ in 0..16 {
        let windows = cng_pair().unwrap();
        let linux = ecdh::pair();
        let on_windows = cng_ecdh(&windows, &ecdh::public(&linux)).unwrap();
        let on_linux = ecdh::agree(&linux, &cng_public(&windows)).unwrap();
        assert_eq!(on_windows, on_linux);
        // Each side agrees with itself the same way too, so a key pair
        // can move between them.
        assert_eq!(
            on_linux,
            ecdh::agree(&windows, &ecdh::public(&linux)).unwrap()
        );
        assert_eq!(on_windows, cng_ecdh(&linux, &cng_public(&windows)).unwrap());
    }
}

#[test]
fn a_pc_and_a_node_show_the_same_code_and_keep_the_same_key() {
    let (asker, giver) = (cng_pair().unwrap(), ecdh::pair());
    let (asker_public, giver_public) = (cng_public(&asker), ecdh::public(&giver).to_vec());
    let (asker_nonce, giver_nonce) = ([7u8; 16], [9u8; 16]);
    let on_pc = agree(
        &cng_ecdh(&asker, &giver_public).unwrap(),
        (&asker_public, &asker_nonce),
        (&giver_public, &giver_nonce),
    );
    let on_node = agree(
        &ecdh::agree(&giver, &asker_public).unwrap(),
        (&asker_public, &asker_nonce),
        (&giver_public, &giver_nonce),
    );
    assert_eq!(on_pc, on_node);
    assert_eq!(on_pc.code.len(), 7);
    // A relay that put its own key in the middle shows another code.
    let relay = ecdh::pair();
    let fooled = agree(
        &ecdh::agree(&relay, &asker_public).unwrap(),
        (&asker_public, &asker_nonce),
        (&ecdh::public(&relay), &giver_nonce),
    );
    assert_ne!(fooled.code, on_pc.code);
}

#[test]
fn frames_sealed_on_one_open_on_the_other() {
    let link = {
        let mut k = [0u8; 32];
        horadric_link::random(&mut k);
        k
    };
    let key = session_key(&link, &[1; 16], &[2; 16]);
    let aad = frame_aad(Way::FromAsker, 41);
    let plain = br#"{"t":"roster","brothers":[]}"#;

    let from_pc = cng::seal_bound(&key, plain, &aad);
    assert_eq!(
        horadric_link::open_bound(&key, &from_pc, &aad).unwrap(),
        plain
    );
    let from_node = horadric_link::seal_bound(&key, plain, &aad);
    assert_eq!(cng::open_bound(&key, &from_node, &aad).unwrap(), plain);

    // Unbound, as a phone's frames are, and empty.
    let from_pc = cng::seal(&key, b"");
    assert_eq!(horadric_link::open(&key, &from_pc).unwrap(), b"");
    let from_node = horadric_link::seal(&key, b"terminal bytes");
    assert_eq!(cng::open(&key, &from_node).unwrap(), b"terminal bytes");

    // The same nonce gives the same bytes, AES-128 for Web Push included.
    let nonce = [3u8; 12];
    assert_eq!(
        cng::gcm(&key, &nonce, plain),
        horadric_link::gcm(&key, &nonce, plain)
    );
    assert_eq!(
        cng::gcm(&key[..16], &nonce, plain),
        horadric_link::gcm(&key[..16], &nonce, plain)
    );
}

#[test]
fn a_tampered_or_misbound_frame_opens_on_neither() {
    let key = [5u8; 32];
    let aad = frame_aad(Way::FromGiver, 7);
    for sealed in [
        cng::seal_bound(&key, b"let me in", &aad),
        horadric_link::seal_bound(&key, b"let me in", &aad),
    ] {
        for i in [0, 12, sealed.len() - 1] {
            let mut bent = sealed.clone();
            bent[i] ^= 0x80;
            assert_eq!(cng::open_bound(&key, &bent, &aad), None, "byte {i}");
            assert_eq!(
                horadric_link::open_bound(&key, &bent, &aad),
                None,
                "byte {i}"
            );
        }
        // Played back as another frame number, or the other way.
        for wrong in [frame_aad(Way::FromGiver, 8), frame_aad(Way::FromAsker, 7)] {
            assert_eq!(cng::open_bound(&key, &sealed, &wrong), None);
            assert_eq!(horadric_link::open_bound(&key, &sealed, &wrong), None);
        }
        assert_eq!(cng::open_bound(&[6; 32], &sealed, &aad), None);
        assert_eq!(horadric_link::open_bound(&[6; 32], &sealed, &aad), None);
    }
}
