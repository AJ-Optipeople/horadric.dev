//! Proof that CNG and `horadric-link` speak the same bytes: a PC (CNG)
//! and a Linux node (`horadric-link`) must agree on keys, codes and
//! sealed frames, or a node could never join. Each test runs one side on
//! each implementation, both ways round, so neither can drift alone.

use horadric_link::ecdh;

use serde_json::json;

use crate::brothers::Cng;
use crate::joining::{agree, frame_aad, join_session_key, Crypto, Handshake, Held, Me, Out, Way};
use crate::portal_crypto as cng;
use crate::portal_push::{ecdh as cng_ecdh, ecdh_pair as cng_pair};

/// The crypto a Linux node joins with, as `horadric node` wires it.
struct Link;

impl Crypto for Link {
    fn random(&self, buf: &mut [u8]) {
        horadric_link::random(buf);
    }

    fn pair(&self) -> Option<[u8; 96]> {
        Some(ecdh::pair())
    }

    fn ecdh(&self, private: &[u8; 96], peer: &[u8]) -> Option<[u8; 32]> {
        ecdh::agree(private, peer).ok()
    }

    fn seal_bound(&self, key: &[u8; 32], plain: &[u8], aad: &[u8]) -> Vec<u8> {
        horadric_link::seal_bound(key, plain, aad)
    }

    fn open_bound(&self, key: &[u8; 32], sealed: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
        horadric_link::open_bound(key, sealed, aad)
    }

    fn sign(&self, private: &[u8; 96], msg: &[u8]) -> Option<[u8; 64]> {
        horadric_link::ecdsa::sign(private, msg)
    }

    fn verify(&self, public: &[u8], msg: &[u8], sig: &[u8]) -> bool {
        horadric_link::ecdsa::verify(public, msg, sig)
    }
}

/// The sealed frames among what a handshake step says.
fn sealed(out: &[Out]) -> Vec<Vec<u8>> {
    out.iter()
        .filter_map(|o| match o {
            Out::Sealed(b) => Some(b.clone()),
            _ => None,
        })
        .collect()
}

/// Passes what one side sealed to the other, as the relay would.
fn pass(out: &[Out], c: &dyn Crypto, to: &mut Handshake) -> Vec<Out> {
    sealed(out)
        .iter()
        .flat_map(|b| to.sealed(c, b, 0).1)
        .collect()
}

/// A PC and a node link again under the hellos bound to both ends, each
/// side on its own crypto, both ways round; and on either crypto, a
/// machine handed its own hello by the relay answers nothing.
#[test]
fn a_pc_and_a_node_relink_bound_to_both_ends() {
    let mut link = [0u8; 32];
    horadric_link::random(&mut link);
    let pc = cng_public(&cng_pair().unwrap());
    let node = ecdh::public(&ecdh::pair()).to_vec();
    let sides: [(&dyn Crypto, &str, &[u8]); 2] = [(&Cng, "pc", &pc), (&Link, "node", &node)];
    for (asker, giver) in [(0, 1), (1, 0)] {
        let (ac, aid, apub) = sides[asker];
        let (gc, gid, gpub) = sides[giver];
        let me_a = Me {
            id: aid,
            name: aid,
            public: apub,
        };
        let me_g = Me {
            id: gid,
            name: gid,
            public: gpub,
        };
        let held = |public: &[u8]| Held {
            link,
            public: public.to_vec(),
        };
        let (mut a, hello) = Handshake::link(ac, me_a, (gid, gid), held(gpub), [1; 16]);
        let mut g = Handshake::give(me_g, (aid, aid), Some(held(apub)), [2; 16]);
        let answer = pass(&hello, gc, &mut g);
        let ready = pass(&answer, ac, &mut a);
        assert!(a.linked(), "{aid} asking {gid}");
        pass(&ready, gc, &mut g);
        assert!(g.linked(), "{gid} giving to {aid}");
        let f = a.seal(ac, &json!({"t": "x"})).unwrap();
        assert_eq!(g.sealed(gc, &f, 0).0, Some(json!({"t": "x"})));
        let f = g.seal(gc, &json!({"t": "y"})).unwrap();
        assert_eq!(a.sealed(ac, &f, 0).0, Some(json!({"t": "y"})));

        // The relay hands the asker its own hello, as if from its brother.
        let (_, hello) = Handshake::link(ac, me_a, (gid, gid), held(gpub), [3; 16]);
        let mut fake = Handshake::give(me_a, (gid, gid), Some(held(gpub)), [4; 16]);
        assert!(pass(&hello, ac, &mut fake).is_empty());
        assert!(!fake.linked());
    }
}

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
    let key = join_session_key(&link, &[1; 16], &[2; 16]);
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

/// A brother's word is signed with its long term key, ECDH's pair, and
/// any brother checks it whatever it runs: so what CNG signs the link
/// verifies, what the link signs CNG verifies, each under the other's
/// key pairs too, and a signature under another key or of another word
/// verifies on neither.
#[test]
fn signatures_made_on_one_verify_on_the_other() {
    let sides: [&dyn Crypto; 2] = [&Cng, &Link];
    for _ in 0..8 {
        for pair in [cng_pair().unwrap(), ecdh::pair()] {
            let public = cng_public(&pair);
            let other = cng_public(&cng_pair().unwrap());
            for signer in sides {
                let sig = signer.sign(&pair, b"I hand s to b at 3").unwrap();
                for checker in sides {
                    assert!(checker.verify(&public, b"I hand s to b at 3", &sig));
                    assert!(!checker.verify(&public, b"I hand s to c at 3", &sig));
                    assert!(!checker.verify(&other, b"I hand s to b at 3", &sig));
                    let mut bent = sig;
                    bent[40] ^= 1;
                    assert!(!checker.verify(&public, b"I hand s to b at 3", &bent));
                }
            }
        }
    }
}
