use std::io::Cursor;

use super::constants::{HALFKA_HM_DIMENSIONS, NNUE_VERSION_LAYERSTACK_NUM_BUCKETS};
#[cfg(feature = "layerstack-arch")]
use super::{LayerStacksNetwork, NNUENetwork};

pub(super) fn dense_concat_arch(ft: usize, l1: usize, l2: usize) -> String {
    let ft2 = 2 * ft;
    let l2_in = 2 * l1;
    let c = l2_in + 2 * l2;
    format!(
        "Features=HalfKaHmMerged(Friend)[{HALFKA_HM_DIMENSIONS}->{ft}x2],Head=DenseConcat,Network=AffineTransform[1<-{c}](DenseConcat[{c}](AffineTransform[{l2}<-{l2_in}](SqrClippedReLU[{l2_in}](AffineTransform[{l1}<-{ft2}](InputSlice[{ft2}(0:{ft2})]))))),fv_scale=28"
    )
}

fn header(arch: &str) -> Vec<u8> {
    let mut bytes = NNUE_VERSION_LAYERSTACK_NUM_BUCKETS.to_le_bytes().to_vec();
    bytes.extend_from_slice(&0x4443_4e43_u32.to_le_bytes());
    bytes.extend_from_slice(&(arch.len() as u32).to_le_bytes());
    bytes.extend_from_slice(arch.as_bytes());
    bytes
}

#[cfg(feature = "layerstack-arch")]
fn static_read_error(arch: &str) -> String {
    LayerStacksNetwork::read_with_options(&mut Cursor::new(header(arch)), 2304, 16, 64, None)
        .err()
        .expect("header should fail before the FT payload")
        .to_string()
}

#[test]
fn dense_concat_rejects_unknown_head_and_layout_tools_reject_dense_concat() {
    let arch = dense_concat_arch(2304, 16, 64);
    for head in ["Unknown", "SplitSkip"] {
        let unknown = arch.replace("Head=DenseConcat", &format!("Head={head}"));
        let expected = format!("Unknown LayerStacks Head={head}");
        #[cfg(feature = "layerstack-arch")]
        assert!(static_read_error(&unknown).contains(&expected));
        assert!(
            super::detect_format(&header(&unknown), 0)
                .unwrap_err()
                .to_string()
                .contains(&expected)
        );
    }

    let bytes = header(&arch);
    let info = super::detect_format(&bytes, bytes.len() as u64).unwrap();
    assert_eq!(info.architecture, "LayerStacks-DenseConcat");
    assert_eq!(info.arch_string, arch);
    let error = super::net_bin_layout::LayerStacksBinLayout::from_bytes(&bytes).unwrap_err();
    assert!(error.to_string().contains("does not support Head=DenseConcat"));

    #[cfg(feature = "nnue-runtime-dimensions")]
    {
        let error = super::dynamic_layer_stacks::DynamicLayerStacksNetwork::read_with_source(
            &mut Cursor::new(&bytes),
            None,
            #[cfg(feature = "prepacked-nnue")]
            None,
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("Dynamic LayerStacks reader"));
    }
}

#[test]
fn dense_concat_requires_layerstack_arch_to_load() {
    let arch = dense_concat_arch(2304, 16, 64);
    let err = super::network::validate_read_head(&arch, false).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert!(err.to_string().contains("requires a static DenseConcat edition"));

    #[cfg(not(feature = "layerstack-arch"))]
    {
        let err = super::NNUENetwork::read(&mut Cursor::new(header(&arch))).err().unwrap();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("requires a static DenseConcat edition"));
    }
}

#[cfg(all(
    feature = "layerstack-arch",
    not(feature = "layerstacks-dense-concat-2304x16x64")
))]
#[test]
fn dense_concat_is_rejected_by_split_skip_build() {
    let arch = dense_concat_arch(2304, 16, 64);
    assert!(static_read_error(&arch).contains("SplitSkip LayerStacks build"));
    let mut bytes = header(&arch);
    bytes.extend_from_slice(&9u32.to_le_bytes());
    let error = NNUENetwork::read(&mut Cursor::new(bytes)).err().unwrap();
    assert!(error.to_string().contains("does not support Head=DenseConcat"));
}

#[cfg(feature = "layerstacks-dense-concat-2304x16x64")]
#[test]
fn dense_concat_build_rejects_split_skip_and_mismatched_dimensions() {
    let split_skip_arch = format!(
        "Features=HalfKaHmMerged(Friend)[{HALFKA_HM_DIMENSIONS}->2304x2],LayerStacks,l2=16,l3=64"
    );
    assert!(static_read_error(&split_skip_arch).contains("model head is SplitSkip"));
    let error = NNUENetwork::read(&mut Cursor::new(header(&split_skip_arch))).err().unwrap();
    assert!(error.to_string().contains("model head is SplitSkip"));

    for (ft, l1, l2) in [(1536, 16, 64), (2304, 32, 64), (2304, 16, 32)] {
        let error = NNUENetwork::read(&mut Cursor::new(header(&dense_concat_arch(ft, l1, l2))))
            .err()
            .unwrap();
        assert!(error.to_string().contains("DenseConcat dimensions (FT, L1, L2) mismatch"));
    }
    let wrong_concat =
        dense_concat_arch(2304, 16, 64).replace("DenseConcat[160]", "DenseConcat[128]");
    assert!(static_read_error(&wrong_concat).contains("Invalid DenseConcat network architecture"));
}

#[cfg(feature = "layerstacks-dense-concat-2304x16x64")]
#[test]
fn dense_concat_rejects_noncanonical_arch_tokens() {
    let arch = dense_concat_arch(2304, 16, 64);
    for malformed in [
        arch.replace(",Head=", ",Network=Duplicate,Head="),
        arch.replace(",Head=", ",PSQT=9,PSQT=9,Head="),
        arch.replace(",Head=", ",Unknown=1,Head="),
        arch.replace(",Head=", ",ThreatProfile=1,Head="),
        arch.replace("HalfKaHmMerged(Friend)", "HalfKaHmMerged(Enemy)"),
        arch.replace("73305", "1"),
        arch.replace(",fv_scale=28", ",fv_scale=28,fv_scale=28"),
        format!("{arch},ThreatWidth=768"),
        format!("{arch},"),
    ] {
        let err = static_read_error(&malformed);
        assert!(err.contains("Invalid DenseConcat network architecture"), "{malformed}: {err}");
    }
}

#[cfg(feature = "layerstacks-dense-concat-2304x16x64")]
#[test]
fn dense_concat_file_forward_matches_scalar_and_refresh_matches_update() {
    use rand::{Rng, SeedableRng};
    use rand_xoshiro::Xoshiro256PlusPlus;

    use super::leb128::{LEB128_MAGIC, encode_signed_leb128};
    use super::{
        AccumulatorLayerStacks, HalfKaHmMergedSpec, LsSaturationCounts, NetworkLayerStacks,
    };
    use crate::position::{Position, SFEN_HIRATE};
    use crate::types::Move;

    struct Affine {
        input_width: usize,
        biases: Vec<i32>,
        weights: Vec<i8>,
    }

    impl Affine {
        fn random(rng: &mut impl Rng, input_width: usize, output_width: usize) -> Self {
            Self {
                input_width,
                biases: (0..output_width).map(|_| rng.random_range(-8000..8000)).collect(),
                weights: (0..input_width * output_width)
                    .map(|_| rng.random_range(-32..32))
                    .collect(),
            }
        }

        fn write(&self, bytes: &mut Vec<u8>) {
            bytes.extend(self.biases.iter().flat_map(|bias| bias.to_le_bytes()));
            for weights in self.weights.chunks_exact(self.input_width) {
                bytes.extend(weights.iter().map(|&w| w as u8));
                bytes.resize(
                    bytes.len() + self.input_width.next_multiple_of(32) - self.input_width,
                    0,
                );
            }
        }

        fn eval(&self, input: &[u8]) -> Vec<i32> {
            assert_eq!(input.len(), self.input_width);
            self.biases
                .iter()
                .zip(self.weights.chunks_exact(self.input_width))
                .map(|(&bias, row)| {
                    bias + row
                        .iter()
                        .zip(input)
                        .map(|(&w, &x)| i32::from(w) * i32::from(x))
                        .sum::<i32>()
                })
                .collect()
        }
    }

    fn activations(xs: &[i32]) -> impl Iterator<Item = u8> + '_ {
        xs.iter()
            .map(|&x| (i64::from(x).pow(2) / 524_288).min(127) as u8)
            .chain(xs.iter().map(|&x| (x / 64).clamp(0, 127) as u8))
    }

    const FT: usize = 64;
    const BUCKETS: usize = 2;
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x4443_4e43);
    let arch = format!("{},ls_bucket=ownking:5", dense_concat_arch(FT, 16, 64));
    let mut bytes = header(&arch);
    bytes.extend_from_slice(&(BUCKETS as u32).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let mut ft = Vec::new();
    for _ in 0..FT {
        encode_signed_leb128(rng.random_range(60..140), &mut ft);
    }
    for _ in 0..FT * HALFKA_HM_DIMENSIONS {
        encode_signed_leb128(rng.random_range(-4..5), &mut ft);
    }
    bytes.extend_from_slice(LEB128_MAGIC);
    bytes.extend_from_slice(&(ft.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&ft);

    let params: [[Affine; 3]; BUCKETS] = std::array::from_fn(|_| {
        let layers = [(FT, 16), (32, 64), (160, 1)].map(|(i, o)| Affine::random(&mut rng, i, o));
        bytes.extend_from_slice(&0x4443_4e43_u32.to_le_bytes());
        for layer in &layers {
            layer.write(&mut bytes);
        }
        layers
    });
    let mut truncated = &bytes[..bytes.len() - 1];
    type Net = NetworkLayerStacks<FT, 16, 30, 32, HalfKaHmMergedSpec, 64>;
    assert!(Net::read(&mut Cursor::new(&mut truncated)).is_err());
    let net = Net::from_bytes(&bytes).unwrap();
    let mut pos = Position::new();
    pos.set_sfen(SFEN_HIRATE).unwrap();
    let mut acc = AccumulatorLayerStacks::new();
    net.refresh_accumulator(&pos, &mut acc);

    for move_text in [None, Some("7g7f"), Some("3c3d"), Some("5i6h")] {
        if let Some(move_text) = move_text {
            let mv = pos.to_move(Move::from_usi(move_text).unwrap()).unwrap();
            assert!(pos.is_legal(mv));
            let dirty = pos.do_move(mv, pos.gives_check(mv));
            let mut updated = AccumulatorLayerStacks::new();
            net.update_accumulator(&pos, &dirty, &mut updated, &acc);
            acc = updated;
        }
        let mut refreshed = AccumulatorLayerStacks::new();
        net.refresh_accumulator(&pos, &mut refreshed);
        assert_eq!(acc.get(0), refreshed.get(0));
        assert_eq!(acc.get(1), refreshed.get(1));

        let side = pos.side_to_move();
        let input: Vec<_> = [side, !side]
            .into_iter()
            .flat_map(|side| {
                let values = refreshed.get(side as usize);
                let (a, b) = values.split_at(FT / 2);
                a.iter().zip(b).map(|(&a, &b)| (a.clamp(0, 127) * b.clamp(0, 127) / 128) as u8)
            })
            .collect();
        for (bucket, [fc0, fc1, fc2]) in params.iter().enumerate() {
            let o0 = fc0.eval(&input);
            let c0: Vec<_> = activations(&o0).collect();
            let o1 = fc1.eval(&c0);
            let c = c0.iter().copied().chain(activations(&o1)).collect::<Vec<_>>();
            let expected = fc2.eval(&c)[0] + o0[14] - o0[15];
            let (score, raw) = net.evaluate_with_bucket_and_raw(&pos, &acc, bucket);
            assert_eq!(raw, expected, "{move_text:?}, bucket {bucket}");
            assert_eq!(score, crate::types::Value::new(expected / 28));
            if net.bucket_spec().unwrap().bucket(&pos) == bucket {
                assert_eq!(net.evaluate(&pos, &acc), score);
            }
            let mut counts = LsSaturationCounts::default();
            net.accumulate_saturation_counts(&acc, side, bucket, &mut counts);
            assert_eq!(counts.l1_act_total, 32);
            assert_eq!(counts.l2_act_total, 128);
            assert_eq!(counts.l1_act_sat, c0.iter().filter(|&&x| x == 127).count() as u64);
            assert_eq!(counts.l2_act_sat, c[32..].iter().filter(|&&x| x == 127).count() as u64);
        }
    }
}
