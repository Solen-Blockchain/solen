//! Post-quantum signatures — ML-DSA-65 (FIPS 204).
//!
//! An opt-in, quantum-resistant alternative to Ed25519 for high-assurance smart
//! accounts (`AuthMethod::MlDsa`). Shor's algorithm lets a quantum computer
//! recover an Ed25519/ECDSA private key from its public key; ML-DSA is a
//! module-lattice scheme with no known quantum break. ML-DSA-65 is NIST security
//! category 3.
//!
//! Verification is a deterministic pure function, so it is safe to run inside
//! consensus (every node reaches the same verdict). Signing is "hedged"
//! (randomized) and happens client-side only — it never runs on a validator —
//! so the signing randomness does not affect consensus. Ed25519 remains the
//! default everywhere; this is purely additive.

use fips204::ml_dsa_65;
use fips204::traits::{KeyGen, SerDes, Signer, Verifier};

use crate::SigningError;

/// ML-DSA-65 public-key length in bytes (1952).
pub const ML_DSA_PK_LEN: usize = ml_dsa_65::PK_LEN;
/// ML-DSA-65 signature length in bytes (3309).
pub const ML_DSA_SIG_LEN: usize = ml_dsa_65::SIG_LEN;

/// FIPS 204 context string. Left empty: the message signed here is the
/// operation's signing digest, which the caller already binds to the chain id /
/// domain, and an empty context keeps interoperability with standard ML-DSA
/// tooling. (Must match between sign and verify — both use this constant.)
const CTX: &[u8] = b"";

/// An ML-DSA-65 keypair (post-quantum). Used for client-side signing only.
pub struct MlDsaKeypair {
    sk: ml_dsa_65::PrivateKey,
    pk: ml_dsa_65::PublicKey,
}

impl MlDsaKeypair {
    /// Generate a new random keypair using the OS CSPRNG.
    pub fn generate() -> Self {
        let mut rng = rand::thread_rng();
        let (pk, sk) = ml_dsa_65::try_keygen_with_rng(&mut rng).expect("ml-dsa-65 keygen failed");
        Self { sk, pk }
    }

    /// Deterministically derive a keypair from a 32-byte seed (the FIPS 204 ξ),
    /// so a wallet can persist a single 32-byte secret exactly like Ed25519.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        let (pk, sk) = ml_dsa_65::KG::keygen_from_seed(seed);
        Self { sk, pk }
    }

    /// The serialized public key (`ML_DSA_PK_LEN` bytes) — stored in
    /// `AuthMethod::MlDsa { public_key }`.
    pub fn public_key(&self) -> Vec<u8> {
        self.pk.clone().into_bytes().to_vec()
    }

    /// Sign a message, returning an `ML_DSA_SIG_LEN`-byte signature.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.sk
            .try_sign(message, CTX)
            .expect("ml-dsa-65 sign failed")
            .to_vec()
    }
}

/// Verify an ML-DSA-65 signature against a public key and message.
///
/// Deterministic — safe to run in consensus. Wrong-length keys or signatures
/// are rejected up front (never panics on attacker input).
pub fn verify_ml_dsa(
    public_key: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Result<(), SigningError> {
    let pk_arr: [u8; ML_DSA_PK_LEN] = public_key
        .try_into()
        .map_err(|_| SigningError::InvalidPublicKey)?;
    let sig_arr: [u8; ML_DSA_SIG_LEN] = signature
        .try_into()
        .map_err(|_| SigningError::InvalidSignature)?;
    let pk = ml_dsa_65::PublicKey::try_from_bytes(pk_arr)
        .map_err(|_| SigningError::InvalidPublicKey)?;
    if pk.verify(message, &sig_arr, CTX) {
        Ok(())
    } else {
        Err(SigningError::InvalidSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() {
        let kp = MlDsaKeypair::generate();
        let pk = kp.public_key();
        assert_eq!(pk.len(), ML_DSA_PK_LEN);
        let msg = b"hello quantum solen";
        let sig = kp.sign(msg);
        assert_eq!(sig.len(), ML_DSA_SIG_LEN);
        assert!(verify_ml_dsa(&pk, msg, &sig).is_ok());
    }

    #[test]
    fn wrong_message_fails() {
        let kp = MlDsaKeypair::generate();
        let sig = kp.sign(b"correct");
        assert!(verify_ml_dsa(&kp.public_key(), b"wrong", &sig).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let kp1 = MlDsaKeypair::generate();
        let kp2 = MlDsaKeypair::generate();
        let sig = kp1.sign(b"msg");
        assert!(verify_ml_dsa(&kp2.public_key(), b"msg", &sig).is_err());
    }

    #[test]
    fn from_seed_is_deterministic() {
        let seed = [7u8; 32];
        let a = MlDsaKeypair::from_seed(&seed);
        let b = MlDsaKeypair::from_seed(&seed);
        assert_eq!(a.public_key(), b.public_key());
        // A signature from one verifies under the other's (same) key.
        let sig = a.sign(b"x");
        assert!(verify_ml_dsa(&b.public_key(), b"x", &sig).is_ok());
    }

    #[test]
    fn malformed_inputs_rejected_not_panic() {
        let kp = MlDsaKeypair::generate();
        let sig = kp.sign(b"m");
        assert!(verify_ml_dsa(b"tooshort", b"m", &sig).is_err());
        assert!(verify_ml_dsa(&kp.public_key(), b"m", b"shortsig").is_err());
    }
}

#[cfg(test)]
mod xvectors {
    use super::*;
    /// Emits cross-impl test vectors (run: `cargo test -p solen-crypto emit_pq -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn emit_pq_vectors() {
        use solen_types::transaction::{Action, UserOperation};
        let op = UserOperation {
            sender: [1u8; 32],
            nonce: 42,
            actions: vec![
                Action::Transfer { to: [2u8; 32], amount: 500 },
                Action::Call { target: [3u8; 32], method: "increment".to_string(), args: vec![9, 8, 7] },
            ],
            max_fee: 1_000_000,
            signature: vec![],
        };
        let msg = op.signing_message(1337);
        let seed = [7u8; 32];
        let kp = MlDsaKeypair::from_seed(&seed);
        let pk = kp.public_key();
        let sig = kp.sign(&msg);
        let hx = |b: &[u8]| b.iter().map(|x| format!("{:02x}", x)).collect::<String>();
        assert!(verify_ml_dsa(&pk, &msg, &sig).is_ok());
        println!("VEC_MSG={}", hx(&msg));
        println!("VEC_PK={}", hx(&pk));
        println!("VEC_SIG={}", hx(&sig));
    }
}

#[cfg(test)]
mod ts_cross {
    use super::*;
    #[test]
    fn rust_verifies_ts_produced_ml_dsa_signature() {
        use solen_types::transaction::{Action, UserOperation};
        let op = UserOperation {
            sender: [1u8; 32], nonce: 42,
            actions: vec![
                Action::Transfer { to: [2u8; 32], amount: 500 },
                Action::Call { target: [3u8; 32], method: "increment".to_string(), args: vec![9, 8, 7] },
            ],
            max_fee: 1_000_000, signature: vec![],
        };
        let msg = op.signing_message(1337);
        let pk = MlDsaKeypair::from_seed(&[7u8; 32]).public_key();
        let ts_sig: Vec<u8> = (0..6618).step_by(2)
            .map(|i| u8::from_str_radix(&TS_SIG_HEX[i..i+2], 16).unwrap()).collect();
        assert!(verify_ml_dsa(&pk, &msg, &ts_sig).is_ok(),
            "the Rust node MUST accept a TS-produced ML-DSA signature");
    }
    const TS_SIG_HEX: &str = "c7771dfbcc4e76bec749f9ec58dccaad1a7c2731cd339b492ad2d3721ba81f3e576f2aa8a582b655e651e81e16443e1935ee7a300f8e9351ce93bee7a941712657804e076e20ac20ecd3fa298d72118d449eee26e87e7bb7baaf1cfce1ccd9030faa9044315bad51dd34a0b3cbf33e032a7c4b36bd2ceee7853b05ea7f959c741cebeb51fffc94cdd352553a289cf84d875ce5539159c198be31a072a67e0a123b8af87095ac7291080c1fb379b3f8277a301eaed9746ad93ae3f4371958abe90941ccc90942991f43626e3dbc3e7d74b48d8d1e1a37b296dce1366cf636a0351236e599180087cf10cb86d01ca83734a5c510258e42124c87431016ff0b546337869687a180dc189d59a8e8cdf6a7d4b0b0220a2da3b906197321ce78a1e97a3db5b1589fc3cd0b7845c8fb5e552e551072bca0262f448f236b3fd4c53de9c6738dfcd02eb5c4dbed829f5cf1b20cf7046d38dc0d9908f2e24d89e555a48399489b2ec5b58f0b707768d26976ad9bda81a580f3ed4fd0121c37ed4c0021ffb533955e733d3a40623096a67ec139fec6de358ba0056912703f3bb4e50ea87d525563f773c98212e467b404b5e07a240a7b95af91fb2567a8b89f6bd47db2d8a121504437391eca613310cc364e417efe89c151bcb00c60c7c303d9a204f5055510980aadee724509d0fadd1cc77b81a65e91c2353f98a8573e17d2acca6a6866f3eafbebf63f6356fe0759ebd71e27ec10790dac25a0dc136b9cd750f45a8f40784d787a45517be4cfee364d3ee4b04a30b74a3fda6168e228bcec7fa74372e77f812535b1939bf7282faf59a969aff450bbeeafd61cb11076ff417ac79533985919e1a0635e4a5a47781e7b94dc176950abef32291e988b07d4ec6312fc2758145c0d79de30033ba0320128c76376e86fa3fb67ac801781c6048f1ac572f6c0d62940c733d688d7c5db151b1704021480c3a314fa293e0a25c25a4a885fdbee16ef0fa586cc134ae3684a75df0d7f1ca870b4acda0e9307ab548654143d847869b447180177a78d7f13dfa832254b8da1877d6d47875637fb335cfb12fa64bc3cbe2cd3f746a309053ec759e614c4ed1f1a354ae2234eee18aac73d53944d908138546797d9ae1ae7dd6632e031383d3e1d7dc12681c4fffdc6a6e8bba75699bebdd3787abd518629c913b523e952ba49178947b10ef58910299b5b3e10f554265b4e0be43901fb592c46d859f7d74a8c804989b8a1526c8e8c88900bdbe56acc49f444061823691b965431646662fb7746bc4e2f4861348fb05f5c4f3a98c671ada2bb6a7735baf71db777278c0be275aa689ab1521ff8eab81349571ae9b3fecbd4a1538ca616a5d7eca2b809adc8f8e8e39c68ca666926d264f44206d25aad0d905fffa1df3d6094fba410be1710c7a3b3530d384bf0bb78aebd30f8d71a666233825aea48c5933ad215a5613ebe30634c04d4f3db43fe489a10e3a1b206776a06c1cde06992278711558a65e16c42fe1536b0d8047d4bc9872cdecbf0404bd9933ea48a750b69bc21ce640ca69b217d0935c16917d1dd64414cb93847c20c6a14feb3e5e393341393c9e3c19dca57eb6dede265a88e0711ef18d849ce5722797d78e4e53f54b29d3441167683e141733e1c5a1c72bc7100bdc94d18438191c4bad05feb055366221f32574ac40e58e3ed395dbc039083b1cbbe503af46fff0aa2eb27d38408273e9303832ea558442ea59cf34c1e0ea2507e5f3245a8c02384a56332e13ef6ae158148f3cdf82d2f9596461b0036c57bff71fbd3501ca159ab9049e9cae379cb9c7648166df0158198fe165b5d40e0aa0a523203fe4d26fd56deb7323aa9b63e7ca30953f03f66dc00239935df55a2d2f1e4b51be7b63b6e514fa92e7e0c61120c24144cfec20bc068ccbeacd21c8b5404fc7f6639d42b344c548f5e4b1de43d911c257e9ccf6b4e8b2743e08f9f9b150688e4ec299f92da450123e29d02a0c5372d2d2d1b4fbabdf9d9f4d8d4ce4f907297e90bb936eec0f02ff22863e91ba566eceb03f82cfa618f3decae45637684b84f0cc0e9f7dda89498c0c52bea894242948391136177cc767113f45522be2f233bf2cb0aa6a7e7e57214b3ba08a7bc7364fbd3b8a0f6958a2c2a754889a05939cc3e64bab3b540c60cf7cdfce1d679e2a3c0f42360b21bd5f591eb99f843242f0a696da5498738b4c68daee7539a7d3f7eb347f205afeff4e0a976d1033ad478ca0547404bc4f3031d42b915b4c57f23c12b310c1ff9d411985ca6ceefb3cf4fc79f3c99d52c4cf267b7750785ec879ea9a7a5819a06e19fea8ed8e56ea261b81912cf41b3b2ab9144a8c0508b3c781ef3d89ecd30a2acc9969cf989ba28f26711eebfd69400382849c3eac3981a867eb5797f10d7b320c6c4174eb3284dd32c0305a8d7ce0f07e4e4e8ad2728ff93038a281ab40b42f6b0b03e862b9a62db6d3e0b0293d111fdf59c13e923c78c8b836c31f005069b3781f0638b11555c7e5d59bbdd4a5cabefda9585d5103cf0f484120c075e9924f3daa313a3c56338c5f336046264d1b51c83957c87a7ae5213fa0addbb5ff18217595841ebeb3e43fe2941d5fc7bded8c5aefa6ced4f122cf029d37f08f97fb647d1eef3b8d82e370699327ed0019aa640b9fa5f9b83d47d3a3be5576386f4886e379502f10e497ae142bd0bf316a1c8b801e1dd3d8b238dfe692020483f17a213ca80da13cd7af7e5de9b9eab02fbcb203eb90a9ff1923ff7cfe5f63cfd6e6c938dc9defe34868d378e9b10e7ec683ad77121dbc2bd4bae0ee405f77b238d294d77868dd6f7ce4e4ba2b5d09d43c312481b867867e73ab253a0223e189cee4ca841267c213d517aad16236bac987473f965c78cbfae7f683146089b47de8db2a3e39c90630b6078bc9bc024f50e3da57765aeb0ba8d1d0a0dfda9eacaaef60136e7cf282fb3fae7c041cd8d803d379e5056e8c886cb232764b9a849b92595b17f32ff30adacebb2b1d8075d8c05a03c9766756e12b163191e7821f672ca7acd6386a63d6e8416fabf26d952903afc9d0deb197ba6207443885fd892ba639ec2fe2fa68d85bd6e325251f2ef8f9760670072625ab8536bb6ea4d4e5c11b1083e80cbffd9275ca89adaa8604542620f14d8acbcc80fca3e25c5aa1cdb8812158fdc025a1f8e843682097de517ab7430b3a41ae3209bd457414106dac0adfbda33250ed9382f9470beea1abb0f2994990fd9dd85cbc6fb8255b1e668cdbcaaf24c375c76b0390e5a77bff2872c08535f2018eaf2b69be9cb3cdd2d4f9ba516e4f58785d8ae1ebbdbb9646d95ee42746fdd8f29e8bc6c803fcc42831aefaf32636a88a5106f900e91e56535b21a01906f92cce08a04095d6ec9642315c96476d760fc2d083f6263dd1d6b6b1eccf70e572f2a25e5400df52c1b359edc316272225bd59514b6582dff5493fdb29b42fcea5ddd173815337f826027b364e6e8b7fb7816fb6b27a1a19d48ef5511631bc5bdad5f7a01dde7321cab2699cd2873bfa51322a5d0a4cd1f416a55d461659c3732cd326067d42e13f77b8be60296d4807d19f2b7c160f4e52e3788384d6ad3a9d577f2e94c7eb0e35665474d78d7c1c419b92953212c88619d57f32a26de3c9536b9e5021dc015824d13e0db53e6b19bf9bf968e9d65f7ec4029a47e90d0d10ffc2d4dfa04d1dab2178c4b871b70573d0e2ff04a2c2d5bde9c2e37fae6f13a899406ec73c35f14d4ee3821005fc5b00f8553254c2768b2c5200e63ee23b314cee07f12c05548b0c24360f24047537ad4bc7173d7782be3ff4d97f306d94aa0c820bd6bae941017882909c495274bfc3abecdf5fbf4ae8eee1bc319439518f8f2799eb89353aa6b851f5d874b9fd42d619f8bd50340f48c341cd6d96b50f10193cf470cc6394a8f2ebb9fb8239462d22669fffc16ec2f21041cd2fca2d143a07f5ad5a6a5487eec6db67804dd32be8212f32e9c1151977cb711bd23959de159909d1158b53ea543d11d55bdcfe2080a1fd432a6bc6fe9a688ee8adb88b78240e5d3ec707351ebb5a41d79b874df22f13b204e3210405eea62155c3332e522f566b78695b19410a6eb9257ba68e8110ffa760cb0f6f16deee1b5feb9b5e82769f6c7a05a877b2266e96455584512345c02d6ac6707626d66352715fc25b652a8ca5ea9436856d6b85137ee6d5ead09dc1c9d3567fda4268bb078164b54975c7f0b87b0418c9e517bd16449fe65df11d6a8dbdbf046120d60c57a0e8b1d224d4f275973d9d0831cc7ceb11fd8d055068cf5ebecf569db08b89eff450f9dc22bdc1a6cba2da13817f483d7c0d8bf390aea44deb80a4a260fd23f57af10ddf9c5fcbb0da6789a7c8de9a0de72c5ed7b295be4a95f654bd60901ecefc7f5d209cddbc038e214b0b909d0ef702d687d07c70827c6af6cd733ead2f0928c1ddfe873757266b7d91b7bc1b1d9d303bfccee405552b2ce2ee2ed73adebd4dff82bc8dd7dccacdbf926424abb43c40a9da118d0b4ce67c5c9ae82bafb4fed965ae5d10bc7a6ceb79c330f8424ccc192a2ab495d774cb73616253639d3e2fc0758def7ff111516a6b12ce5eff0fa215b890860868f90a0a9b800000000000000000000000000000000000000000000070c11161921";
}
