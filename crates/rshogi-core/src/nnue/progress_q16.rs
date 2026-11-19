//! YaneuraOu SFNN モデル用の Q16 整数進行度 routing。tatara の浮動小数点形式とは区別する。
//! 256 段階に量子化して振り分ける BulletOu 形式とは、バケット数が 256 の約数である場合に一致する。
//!
//! 探索用 stack は生成・reset 時に係数の Arc を取得し、探索中はロックなしで参照する。
//! 静的 LayerStacks は視点別の部分和を DirtyPiece から更新する。係数を再設定した場合は
//! 次の探索開始（stack の reset）から反映する。stack を持たない評価 API は都度取得する。
use super::bona_piece::BonaPiece;
use super::bona_piece_halfka_hm_merged::FE_OLD_END;
use super::constants::MAX_LAYER_STACK_BUCKETS;
use super::network::{SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, get_layer_stack_progress_buckets};
use crate::position::Position;
use crate::types::Color;
#[cfg(test)]
use crate::types::PieceType;
use std::sync::{Arc, RwLock};
static WEIGHTS: RwLock<Option<Arc<[i32]>>> = RwLock::new(None);

/// 探索開始時に取得する不変の係数。再設定後も既存の探索は同じ配列を参照する。
pub(crate) fn snapshot_weights() -> Option<Arc<[i32]>> {
    use super::network::{LayerStackBucketMode, get_layer_stack_bucket_mode};
    if get_layer_stack_bucket_mode() != LayerStackBucketMode::ProgressKPAbsQ16 {
        return None;
    }
    WEIGHTS.read().unwrap_or_else(|e| e.into_inner()).clone()
}
// 添字 N ごとの round(ln((i/N) / (1-i/N)) * 65536), i=1..N。
// 初回評価も含めて、閾値の計算・ヒープ確保を評価経路へ持ち込まない。
const THRESHOLDS: [&[i64]; MAX_LAYER_STACK_BUCKETS + 1] = [
    &[], // N=0 は使用しない。
    &[],
    &[0],
    &[-45426, 45426],
    &[-71999, 0, 71999],
    &[-90852, -26573, 26573, 90852],
    &[-105476, -45426, 0, 45426, 105476],
    &[-117425, -60050, -18854, 18854, 60050, 117425],
    &[-127527, -71999, -33477, 0, 33477, 71999, 127527],
    &[-136278, -82101, -45426, -14624, 14624, 45426, 82101, 136278],
    &[
        -143997, -90852, -55529, -26573, 0, 26573, 55529, 90852, 143997,
    ],
    &[
        -150902, -98571, -64280, -36675, -11949, 11949, 36675, 64280, 98571, 150902,
    ],
    &[
        -157148, -105476, -71999, -45426, -22051, 0, 22051, 45426, 71999, 105476, 157148,
    ],
    &[
        -162851, -111722, -78904, -53145, -30802, -10102, 10102, 30802, 53145, 78904, 111722,
        162851,
    ],
    &[
        -168097, -117425, -85150, -60050, -38521, -18854, 0, 18854, 38521, 60050, 85150, 117425,
        168097,
    ],
    &[
        -172953, -122670, -90852, -66296, -45426, -26573, -8751, 8751, 26573, 45426, 66296, 90852,
        122670, 172953,
    ],
    &[
        -177475, -127527, -96098, -71999, -51672, -33477, -16470, 0, 16470, 33477, 51672, 71999,
        96098, 127527, 177475,
    ],
    &[
        -181704, -132049, -100955, -77244, -57375, -39724, -23375, -7719, 7719, 23375, 39724,
        57375, 77244, 100955, 132049, 181704,
    ],
    &[
        -185677, -136278, -105476, -82101, -62620, -45426, -29621, -14624, 0, 14624, 29621, 45426,
        62620, 82101, 105476, 136278, 185677,
    ],
    &[
        -189423, -140251, -109706, -86623, -67477, -50672, -35324, -20870, -6905, 6905, 20870,
        35324, 50672, 67477, 86623, 109706, 140251, 189423,
    ],
    &[
        -192967, -143997, -113679, -90852, -71999, -55529, -40569, -26573, -13151, 0, 13151, 26573,
        40569, 55529, 71999, 90852, 113679, 143997, 192967,
    ],
    &[
        -196328, -147541, -117425, -94825, -76228, -60050, -45426, -31818, -18854, -6246, 6246,
        18854, 31818, 45426, 60050, 76228, 94825, 117425, 147541, 196328,
    ],
    &[
        -199526, -150902, -120968, -98571, -80201, -64280, -49948, -36675, -24099, -11949, 0,
        11949, 24099, 36675, 49948, 64280, 80201, 98571, 120968, 150902, 199526,
    ],
    &[
        -202575, -154100, -124330, -102115, -83947, -68253, -54177, -41196, -28956, -17194, -5702,
        5702, 17194, 28956, 41196, 54177, 68253, 83947, 102115, 124330, 154100, 202575,
    ],
    &[
        -205488, -157148, -127527, -105476, -87491, -71999, -58150, -45426, -33477, -22051, -10948,
        0, 10948, 22051, 33477, 45426, 58150, 71999, 87491, 105476, 127527, 157148, 205488,
    ],
    &[
        -208277, -160062, -130576, -108674, -90852, -75542, -61896, -49399, -37707, -26573, -15805,
        -5246, 5246, 15805, 26573, 37707, 49399, 61896, 75542, 90852, 108674, 130576, 160062,
        208277,
    ],
    &[
        -210952, -162851, -133489, -111722, -94050, -78904, -65440, -53145, -41680, -30802, -20326,
        -10102, 0, 10102, 20326, 30802, 41680, 53145, 65440, 78904, 94050, 111722, 133489, 162851,
        210952,
    ],
    &[
        -213523, -165526, -136278, -114636, -97098, -82101, -68801, -56688, -45426, -34775, -24556,
        -14624, -4857, 4857, 14624, 24556, 34775, 45426, 56688, 68801, 82101, 97098, 114636,
        136278, 165526, 213523,
    ],
    &[
        -215996, -168097, -138954, -117425, -100012, -85150, -71999, -60050, -48969, -38521,
        -28529, -18854, -9378, 0, 9378, 18854, 28529, 38521, 48969, 60050, 71999, 85150, 100012,
        117425, 138954, 168097, 215996,
    ],
    &[
        -218379, -170570, -141524, -120100, -102801, -88063, -75047, -63248, -52331, -42065,
        -32275, -22827, -13608, -4522, 4522, 13608, 22827, 32275, 42065, 52331, 63248, 75047,
        88063, 102801, 120100, 141524, 170570, 218379,
    ],
    &[
        -220679, -172953, -143997, -122670, -105476, -90852, -77961, -66296, -55529, -45426,
        -35818, -26573, -17581, -8751, 0, 8751, 17581, 26573, 35818, 45426, 55529, 66296, 77961,
        90852, 105476, 122670, 143997, 172953, 220679,
    ],
    &[
        -222901, -175253, -146381, -125144, -108046, -93527, -80750, -69209, -58577, -48624,
        -39180, -30116, -21327, -12724, -4230, 4230, 12724, 21327, 30116, 39180, 48624, 58577,
        69209, 80750, 93527, 108046, 125144, 146381, 175253, 222901,
    ],
    &[
        -225050, -177475, -148680, -127527, -110520, -96098, -83425, -71999, -61490, -51672,
        -42377, -33477, -24870, -16470, -8203, 0, 8203, 16470, 24870, 33477, 42377, 51672, 61490,
        71999, 83425, 96098, 110520, 127527, 148680, 177475, 225050,
    ],
    &[
        -227130, -179624, -150902, -129827, -112903, -98571, -85995, -74674, -64280, -54586,
        -45426, -36675, -28232, -20013, -11949, -3973, 3973, 11949, 20013, 28232, 36675, 45426,
        54586, 64280, 74674, 85995, 98571, 112903, 129827, 150902, 179624, 227130,
    ],
    &[
        -229147, -181704, -153051, -132049, -115203, -100955, -88469, -77244, -66955, -57375,
        -48339, -39724, -31429, -23375, -15492, -7719, 0, 7719, 15492, 23375, 31429, 39724, 48339,
        57375, 66955, 77244, 88469, 100955, 115203, 132049, 153051, 181704, 229147,
    ],
    &[
        -231104, -183721, -155132, -134198, -117425, -103254, -90852, -79718, -69525, -60050,
        -51128, -42637, -34478, -26573, -18854, -11262, -3746, 3746, 11262, 18854, 26573, 34478,
        42637, 51128, 60050, 69525, 79718, 90852, 103254, 117425, 134198, 155132, 183721, 231104,
    ],
    &[
        -233003, -185677, -157148, -136278, -119574, -105476, -93152, -82101, -71999, -62620,
        -53804, -45426, -37391, -29621, -22051, -14624, -7289, 0, 7289, 14624, 22051, 29621, 37391,
        45426, 53804, 62620, 71999, 82101, 93152, 105476, 119574, 136278, 157148, 185677, 233003,
    ],
    &[
        -234849, -187577, -159105, -138295, -121654, -107625, -95374, -84401, -74382, -65094,
        -56374, -48101, -40180, -32534, -25100, -17821, -10651, -3543, 3543, 10651, 17821, 25100,
        32534, 40180, 48101, 56374, 65094, 74382, 84401, 95374, 107625, 121654, 138295, 159105,
        187577, 234849,
    ],
    &[
        -236645, -189423, -161005, -140251, -123671, -109706, -97523, -86623, -76682, -67477,
        -58848, -50672, -42856, -35324, -28013, -20870, -13848, -6905, 0, 6905, 13848, 20870,
        28013, 35324, 42856, 50672, 58848, 67477, 76682, 86623, 97523, 109706, 123671, 140251,
        161005, 189423, 236645,
    ],
    &[
        -238393, -191219, -162851, -142151, -125627, -111722, -99603, -88772, -78904, -69777,
        -61231, -53145, -45426, -37999, -30802, -23783, -16897, -10102, -3362, 3362, 10102, 16897,
        23783, 30802, 37999, 45426, 53145, 61231, 69777, 78904, 88772, 99603, 111722, 125627,
        142151, 162851, 191219, 238393,
    ],
    &[
        -240095, -192967, -164646, -143997, -127527, -113679, -101620, -90852, -81052, -71999,
        -63531, -55529, -47899, -40569, -33477, -26573, -19810, -13151, -6559, 0, 6559, 13151,
        19810, 26573, 33477, 40569, 47899, 55529, 63531, 71999, 81052, 90852, 101620, 113679,
        127527, 143997, 164646, 192967, 240095,
    ],
    &[
        -241754, -194669, -166394, -145793, -129373, -115579, -103576, -92869, -83133, -74148,
        -65752, -57828, -50283, -43043, -36048, -29248, -22599, -16064, -9608, -3198, 3198, 9608,
        16064, 22599, 29248, 36048, 43043, 50283, 57828, 65752, 74148, 83133, 92869, 103576,
        115579, 129373, 145793, 166394, 194669, 241754,
    ],
    &[
        -243373, -196328, -168097, -147541, -131169, -117425, -105476, -94825, -85150, -76228,
        -67901, -60050, -52583, -45426, -38521, -31818, -25275, -18854, -12521, -6246, 0, 6246,
        12521, 18854, 25275, 31818, 38521, 45426, 52583, 60050, 67901, 76228, 85150, 94825, 105476,
        117425, 131169, 147541, 168097, 196328, 243373,
    ],
    &[
        -244952, -197947, -169756, -149243, -132917, -119220, -107322, -96725, -87106, -78245,
        -69982, -62199, -54804, -47726, -40905, -34292, -27845, -21529, -15310, -9159, -3049, 3049,
        9159, 15310, 21529, 27845, 34292, 40905, 47726, 54804, 62199, 69982, 78245, 87106, 96725,
        107322, 119220, 132917, 149243, 169756, 197947, 244952,
    ],
    &[
        -246494, -199526, -171374, -150902, -134619, -120968, -109118, -98571, -89006, -80201,
        -71999, -64280, -56953, -49948, -43204, -36675, -30318, -24099, -17985, -11949, -5962, 0,
        5962, 11949, 17985, 24099, 30318, 36675, 43204, 49948, 56953, 64280, 71999, 80201, 89006,
        98571, 109118, 120968, 134619, 150902, 171374, 199526, 246494,
    ],
    &[
        -248001, -201068, -172953, -152520, -136278, -122670, -110866, -100367, -90852, -82101,
        -73955, -66296, -59034, -52097, -45426, -38975, -32702, -26573, -20556, -14624, -8751,
        -2913, 2913, 8751, 14624, 20556, 26573, 32702, 38975, 45426, 52097, 59034, 66296, 73955,
        82101, 90852, 100367, 110866, 122670, 136278, 152520, 172953, 201068, 248001,
    ],
    &[
        -249473, -202575, -174495, -154100, -137897, -124330, -112568, -102115, -92648, -83947,
        -75855, -68253, -61051, -54177, -47575, -41196, -35002, -28956, -23029, -17194, -11426,
        -5702, 0, 5702, 11426, 17194, 23029, 28956, 35002, 41196, 47575, 54177, 61051, 68253,
        75855, 83947, 92648, 102115, 112568, 124330, 137897, 154100, 174495, 202575, 249473,
    ],
    &[
        -250914, -204047, -176002, -155642, -139476, -125948, -114227, -103817, -94396, -85743,
        -77701, -70152, -63007, -56194, -49656, -43345, -37223, -31256, -25413, -19668, -13997,
        -8378, -2789, 2789, 8378, 13997, 19668, 25413, 31256, 37223, 43345, 49656, 56194, 63007,
        70152, 77701, 85743, 94396, 103817, 114227, 125948, 139476, 155642, 176002, 204047, 250914,
    ],
    &[
        -252323, -205488, -177475, -157148, -141018, -127527, -115845, -105476, -96098, -87491,
        -79497, -71999, -64907, -58150, -51672, -45426, -39372, -33477, -27712, -22051, -16470,
        -10948, -5464, 0, 5464, 10948, 16470, 22051, 27712, 33477, 39372, 45426, 51672, 58150,
        64907, 71999, 79497, 87491, 96098, 105476, 115845, 127527, 141018, 157148, 177475, 205488,
        252323,
    ],
    &[
        -253703, -206897, -178915, -158621, -142525, -129069, -117425, -107094, -97757, -89193,
        -81244, -73794, -66753, -60050, -53629, -47443, -41453, -35626, -29934, -24351, -18854,
        -13421, -8035, -2675, 2675, 8035, 13421, 18854, 24351, 29934, 35626, 41453, 47443, 53629,
        60050, 66753, 73794, 81244, 89193, 97757, 107094, 117425, 129069, 142525, 158621, 178915,
        206897, 253703,
    ],
    &[
        -255054, -208277, -180325, -160062, -143997, -130576, -118967, -108674, -99375, -90852,
        -82947, -75542, -68549, -61896, -55529, -49399, -43470, -37707, -32083, -26573, -21153,
        -15805, -10508, -5246, 0, 5246, 10508, 15805, 21153, 26573, 32083, 37707, 43470, 49399,
        55529, 61896, 68549, 75542, 82947, 90852, 99375, 108674, 118967, 130576, 143997, 160062,
        180325, 208277, 255054,
    ],
    &[
        -256378, -209628, -181704, -161471, -145438, -132049, -120473, -110216, -100955, -92470,
        -84606, -77244, -70296, -63692, -57375, -51299, -45426, -39724, -34164, -28721, -23375,
        -18105, -12892, -7719, -2570, 2570, 7719, 12892, 18105, 23375, 28721, 34164, 39724, 45426,
        51299, 57375, 63692, 70296, 77244, 84606, 92470, 100955, 110216, 120473, 132049, 145438,
        161471, 181704, 209628, 256378,
    ],
    &[
        -257676, -210952, -183056, -162851, -146847, -133489, -121946, -111722, -102497, -94050,
        -86224, -78904, -71999, -65440, -59170, -53145, -47326, -41680, -36180, -30802, -25524,
        -20326, -15191, -10102, -5044, 0, 5044, 10102, 15191, 20326, 25524, 30802, 36180, 41680,
        47326, 53145, 59170, 65440, 71999, 78904, 86224, 94050, 102497, 111722, 121946, 133489,
        146847, 162851, 183056, 210952, 257676,
    ],
    &[
        -258949, -212250, -184380, -164202, -148227, -134899, -123387, -113195, -104003, -95592,
        -87803, -80522, -73658, -67142, -60918, -54941, -49172, -43580, -38137, -32819, -27605,
        -22475, -17413, -12402, -7427, -2473, 2473, 7427, 12402, 17413, 22475, 27605, 32819, 38137,
        43580, 49172, 54941, 60918, 67142, 73658, 80522, 87803, 95592, 104003, 113195, 123387,
        134899, 148227, 164202, 184380, 212250, 258949,
    ],
    &[
        -260197, -213523, -185677, -165526, -149578, -136278, -124796, -114636, -105476, -97098,
        -89346, -82101, -75276, -68801, -62620, -56688, -50968, -45426, -40037, -34775, -29621,
        -24556, -19562, -14624, -9727, -4857, 0, 4857, 9727, 14624, 19562, 24556, 29621, 34775,
        40037, 45426, 50968, 56688, 62620, 68801, 75276, 82101, 89346, 97098, 105476, 114636,
        124796, 136278, 149578, 165526, 185677, 213523, 260197,
    ],
    &[
        -261422, -214771, -186950, -166824, -150902, -137630, -126176, -116045, -106917, -98571,
        -90852, -83643, -76855, -70419, -64280, -58391, -52715, -47222, -41883, -36675, -31578,
        -26573, -21643, -16773, -11949, -7156, -2383, 2383, 7156, 11949, 16773, 21643, 26573,
        31578, 36675, 41883, 47222, 52715, 58391, 64280, 70419, 76855, 83643, 90852, 98571, 106917,
        116045, 126176, 137630, 150902, 166824, 186950, 214771, 261422,
    ],
    &[
        -262625, -215996, -188198, -168097, -152200, -138954, -127527, -117425, -108326, -100012,
        -92325, -85150, -78397, -71999, -65898, -60050, -54418, -48969, -43678, -38521, -33477,
        -28529, -23659, -18854, -14098, -9378, -4683, 0, 4683, 9378, 14098, 18854, 23659, 28529,
        33477, 38521, 43678, 48969, 54418, 60050, 65898, 71999, 78397, 85150, 92325, 100012,
        108326, 117425, 127527, 138954, 152200, 168097, 188198, 215996, 262625,
    ],
    &[
        -263805, -217198, -189423, -169345, -153473, -140251, -128851, -118776, -109706, -101421,
        -93765, -86623, -79904, -73541, -67477, -61668, -56077, -50672, -45426, -40317, -35324,
        -30429, -25616, -20870, -16178, -11527, -6905, -2300, 2300, 6905, 11527, 16178, 20870,
        25616, 30429, 35324, 40317, 45426, 50672, 56077, 61668, 67477, 73541, 79904, 86623, 93765,
        101421, 109706, 118776, 128851, 140251, 153473, 169345, 189423, 217198, 263805,
    ],
    &[
        -264965, -218379, -190626, -170570, -154721, -141524, -130149, -120100, -111057, -102801,
        -95175, -88063, -81377, -75047, -69019, -63248, -57695, -52331, -47128, -42065, -37119,
        -32275, -27516, -22827, -18195, -13608, -9054, -4522, 0, 4522, 9054, 13608, 18195, 22827,
        27516, 32275, 37119, 42065, 47128, 52331, 57695, 63248, 69019, 75047, 81377, 88063, 95175,
        102801, 111057, 120100, 130149, 141524, 154721, 170570, 190626, 218379, 264965,
    ],
    &[
        -266105, -219539, -191807, -171772, -155946, -142772, -131422, -121398, -112381, -104152,
        -96555, -89472, -82817, -76520, -70526, -64790, -59274, -53949, -48788, -43767, -38867,
        -34071, -29362, -24726, -20151, -15625, -11135, -6670, -2222, 2222, 6670, 11135, 15625,
        20151, 24726, 29362, 34071, 38867, 43767, 48788, 53949, 59274, 64790, 70526, 76520, 82817,
        89472, 96555, 104152, 112381, 121398, 131422, 142772, 155946, 171772, 191807, 219539,
        266105,
    ],
    &[
        -267225, -220679, -192967, -172953, -157148, -143997, -132670, -122670, -113679, -105476,
        -97906, -90852, -84227, -77961, -71999, -66296, -60817, -55529, -50406, -45426, -40569,
        -35818, -31157, -26573, -22051, -17581, -13151, -8751, -4371, 0, 4371, 8751, 13151, 17581,
        22051, 26573, 31157, 35818, 40569, 45426, 50406, 55529, 60817, 66296, 71999, 77961, 84227,
        90852, 97906, 105476, 113679, 122670, 132670, 143997, 157148, 172953, 192967, 220679,
        267225,
    ],
    &[
        -268327, -221799, -194107, -174113, -158329, -145200, -133895, -123919, -114951, -106774,
        -99230, -92203, -85607, -79370, -73439, -67769, -62323, -57071, -51985, -47044, -42229,
        -37521, -32905, -28368, -23897, -19481, -15108, -10768, -6451, -2149, 2149, 6451, 10768,
        15108, 19481, 23897, 28368, 32905, 37521, 42229, 47044, 51985, 57071, 62323, 67769, 73439,
        79370, 85607, 92203, 99230, 106774, 114951, 123919, 133895, 145200, 158329, 174113, 194107,
        221799, 268327,
    ],
    &[
        -269410, -222901, -195227, -175253, -159489, -146381, -135097, -125144, -116200, -108046,
        -100528, -93527, -86958, -80750, -74848, -69209, -63796, -58577, -53527, -48624, -43847,
        -39180, -34607, -30116, -25693, -21327, -17007, -12724, -8468, -4230, 0, 4230, 8468, 12724,
        17007, 21327, 25693, 30116, 34607, 39180, 43847, 48624, 53527, 58577, 63796, 69209, 74848,
        80750, 86958, 93527, 100528, 108046, 116200, 125144, 135097, 146381, 159489, 175253,
        195227, 222901, 269410,
    ],
    &[
        -270476, -223984, -196328, -176373, -160629, -147541, -136278, -126346, -117425, -109295,
        -101800, -94825, -88282, -82101, -76228, -70619, -65236, -60050, -55034, -50166, -45426,
        -40798, -36267, -31818, -27441, -23123, -18854, -14624, -10424, -6246, -2081, 2081, 6246,
        10424, 14624, 18854, 23123, 27441, 31818, 36267, 40798, 45426, 50166, 55034, 60050, 65236,
        70619, 76228, 82101, 88282, 94825, 101800, 109295, 117425, 126346, 136278, 147541, 160629,
        176373, 196328, 223984, 270476,
    ],
    &[
        -271524, -225050, -197412, -177475, -161749, -148680, -137438, -127527, -118627, -110520,
        -103049, -96098, -89580, -83425, -77580, -71999, -66646, -61490, -56507, -51672, -46968,
        -42377, -37885, -33477, -29143, -24870, -20649, -16470, -12324, -8203, -4097, 0, 4097,
        8203, 12324, 16470, 20649, 24870, 29143, 33477, 37885, 42377, 46968, 51672, 56507, 61490,
        66646, 71999, 77580, 83425, 89580, 96098, 103049, 110520, 118627, 127527, 137438, 148680,
        161749, 177475, 197412, 225050, 271524,
    ],
];
/// raw f64 LE 係数を round(w * 65536) / i32 clamp で量子化する。
pub fn load_progress_coeff_kpabs_q16_from_bytes(bytes: &[u8]) -> Result<Box<[i32]>, String> {
    if bytes.len() != SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS * 8 {
        return Err("progress Q16 coefficient size mismatch".to_string());
    }
    bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|chunk| {
            let value = f64::from_le_bytes(*chunk);
            if !value.is_finite() {
                return Err("progress Q16 coefficient must be finite".to_string());
            }
            Ok((value * 65536.0).round() as i32)
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Vec::into_boxed_slice)
}
/// 設定済みの Q16 係数を bucket mode によらず取得する。stack を持たないツールが
/// 評価ループの外で一度だけ取得し、同じ配列で bucket を数えるために使う。
pub fn get_layer_stack_progress_kpabs_q16_weights() -> Option<Arc<[i32]>> {
    WEIGHTS.read().unwrap_or_else(|e| e.into_inner()).clone()
}
/// Q16 係数を設定する。探索前に routing と合わせて設定すること。
/// 既存の探索用 stack への反映には再生成または reset が必要。
pub fn set_layer_stack_progress_kpabs_q16_weights(weights: Box<[i32]>) -> Result<(), String> {
    if weights.len() != SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS {
        return Err("progress Q16 weights length mismatch".to_string());
    }
    *WEIGHTS.write().map_err(|_| "progress Q16 weights lock poisoned")? = Some(Arc::from(weights));
    crate::eval::changed();
    Ok(())
}
/// Q16 係数を未設定へ戻す。
/// 取得済みの探索用 stack は reset まで元の係数を保持する。
pub fn reset_layer_stack_progress_kpabs_q16_weights() {
    crate::eval::changed();
    *WEIGHTS.write().unwrap_or_else(|e| e.into_inner()) = None;
}
/// Q16 logit 和を bucket へ変換する。閾値と等しい値は上側の bucket に属する。
pub fn progress_q16_sum_to_bucket(sum: i64, num_buckets: usize) -> usize {
    assert!((1..=MAX_LAYER_STACK_BUCKETS).contains(&num_buckets));
    let thresholds = THRESHOLDS[num_buckets];
    thresholds.partition_point(|&threshold| threshold <= sum)
}
pub(crate) fn configured_progress_q16_bucket(pos: &Position, stored_buckets: usize) -> usize {
    let weights = snapshot_weights();
    bucket_with_weights(pos, stored_buckets, weights.as_deref())
}

pub(crate) fn routing_bucket_count(stored_buckets: usize) -> usize {
    let count =
        get_layer_stack_progress_buckets().expect("LayerStacks Q16 routing is not configured");
    assert!(count <= stored_buckets, "Q16 routing exceeds stored buckets");
    count
}

pub(crate) fn bucket_with_weights(
    pos: &Position,
    stored_buckets: usize,
    weights: Option<&[i32]>,
) -> usize {
    let count = routing_bucket_count(stored_buckets);
    if count == 1 {
        return 0;
    }
    let weights = weights.expect("LayerStacks Q16 coefficients are not configured");
    progress_q16_sum_to_bucket(compute_progresskpabs_q16_sum(pos, weights), count)
}
/// 盤上・持駒の両視点の係数を i64 で合算する。float 差分キャッシュは使用しない。
pub fn compute_progresskpabs_q16_sum(pos: &Position, weights: &[i32]) -> i64 {
    assert_eq!(weights.len(), SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS);
    [Color::Black, Color::White]
        .into_iter()
        .map(|perspective| compute_half(pos, perspective, weight_row(pos, perspective, weights)))
        .sum()
}

pub(crate) fn weight_row<'a>(pos: &Position, perspective: Color, weights: &'a [i32]) -> &'a [i32] {
    let king = pos.king_square(perspective);
    let square = if perspective == Color::Black {
        king
    } else {
        king.inverse()
    }
    .index();
    &weights[square * FE_OLD_END..(square + 1) * FE_OLD_END]
}

pub(crate) fn compute_half(pos: &Position, perspective: Color, row: &[i32]) -> i64 {
    let list = if perspective == Color::Black {
        pos.piece_list().piece_list_fb()
    } else {
        pos.piece_list().piece_list_fw()
    };
    list[..super::piece_list::PieceNumber::KING as usize]
        .iter()
        .map(|&bp| coefficient(row, bp))
        .sum()
}

#[inline]
fn coefficient(row: &[i32], bp: BonaPiece) -> i64 {
    // 駒落ちの空き枠は係数ファイルの index 0 の値にかかわらず寄与しない。
    if bp == BonaPiece::ZERO {
        0
    } else {
        i64::from(row[bp.value() as usize])
    }
}

#[cfg(feature = "layerstack-arch")]
pub(crate) fn half_delta(
    dirty: &super::accumulator::DirtyPiece,
    perspective: Color,
    row: &[i32],
) -> i64 {
    let mut delta = 0;
    for i in 0..usize::from(dirty.dirty_num) {
        if dirty.piece_no[i].0 >= super::piece_list::PieceNumber::KING {
            continue;
        }
        let changed = &dirty.changed_piece[i];
        let (old, new) = if perspective == Color::Black {
            (changed.old_piece.fb, changed.new_piece.fb)
        } else {
            (changed.old_piece.fw, changed.new_piece.fw)
        };
        // i32 の端値同士の差も i64 へ拡張してから計算する。
        delta += coefficient(row, new) - coefficient(row, old);
    }
    delta
}

#[cfg(test)]
pub(super) fn reference_board_sums(pos: &Position, weights: &[i32]) -> [i64; 2] {
    assert_eq!(
        weights.len(),
        SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS,
        "progresskpabs weights length mismatch"
    );

    let sq_bk = pos.king_square(Color::Black).index();
    let sq_wk = pos.king_square(Color::White).inverse().index();
    let weights_b = &weights[sq_bk * FE_OLD_END..(sq_bk + 1) * FE_OLD_END];
    let weights_w = &weights[sq_wk * FE_OLD_END..(sq_wk + 1) * FE_OLD_END];

    let mut sum = [0i64; 2];

    for sq in pos.occupied().iter() {
        let pc = pos.piece_on(sq);
        if pc.is_none() || pc.piece_type() == PieceType::King {
            continue;
        }

        let bp_b = BonaPiece::from_piece_square(pc, sq, Color::Black);
        if bp_b != BonaPiece::ZERO {
            sum[0] += i64::from(weights_b[bp_b.value() as usize]);
        }

        let bp_w = BonaPiece::from_piece_square(pc, sq, Color::White);
        if bp_w != BonaPiece::ZERO {
            sum[1] += i64::from(weights_w[bp_w.value() as usize]);
        }
    }

    for owner in [Color::Black, Color::White] {
        let hand = pos.hand(owner);
        for &pt in &PieceType::HAND_PIECES {
            let count = hand.count(pt);
            for c in 1..=count {
                let c_u8 = u8::try_from(c).expect("hand count fits in u8");

                let bp_b = BonaPiece::from_hand_piece(Color::Black, owner, pt, c_u8);
                if bp_b != BonaPiece::ZERO {
                    sum[0] += i64::from(weights_b[bp_b.value() as usize]);
                }

                let bp_w = BonaPiece::from_hand_piece(Color::White, owner, pt, c_u8);
                if bp_w != BonaPiece::ZERO {
                    sum[1] += i64::from(weights_w[bp_w.value() as usize]);
                }
            }
        }
    }

    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_thresholds_match_yo_for_every_supported_bucket_count() {
        for (n, thresholds) in THRESHOLDS.iter().enumerate().skip(1) {
            assert_eq!(thresholds.len(), n - 1);
            assert_eq!(progress_q16_sum_to_bucket(i64::MIN, n), 0);
            assert_eq!(progress_q16_sum_to_bucket(i64::MAX, n), n - 1);
            for i in 1..n {
                let p = i as f64 / n as f64;
                let expected = ((p / (1.0 - p)).ln() * 65536.0).round() as i64;
                assert_eq!(thresholds[i - 1], expected, "N={n} i={i}");
                assert_eq!(progress_q16_sum_to_bucket(expected - 1, n), i - 1);
                assert_eq!(progress_q16_sum_to_bucket(expected, n), i);
                assert_eq!(progress_q16_sum_to_bucket(expected + 1, n), i);
            }
        }
    }

    #[test]
    fn configured_routing_uses_integer_coefficients() {
        use super::super::network::{
            LayerStackBucketMode, configure_layer_stack_routing, layer_stack_routing_test_guard,
            reset_layer_stack_progress_buckets,
        };
        let _guard = layer_stack_routing_test_guard();
        let mut pos = Position::new();
        pos.set_sfen(crate::position::SFEN_HIRATE).unwrap();
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbsQ16, 4, Some(4)).unwrap();
        set_layer_stack_progress_kpabs_q16_weights(
            vec![i32::MAX; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS].into_boxed_slice(),
        )
        .unwrap();
        assert_eq!(configured_progress_q16_bucket(&pos, 4), 3);
        set_layer_stack_progress_kpabs_q16_weights(
            vec![i32::MIN; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS].into_boxed_slice(),
        )
        .unwrap();
        assert_eq!(configured_progress_q16_bucket(&pos, 4), 0);
        reset_layer_stack_progress_kpabs_q16_weights();
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbsQ16, 1, Some(1)).unwrap();
        assert_eq!(configured_progress_q16_bucket(&pos, 1), 0);
        reset_layer_stack_progress_buckets();
    }

    #[test]
    fn quantization_matches_yo_round_and_clamp() {
        let mut bytes = vec![0; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS * 8];
        let values = [
            0.5 / 65536.0,
            -0.5 / 65536.0,
            1.49 / 65536.0,
            -1.49 / 65536.0,
            f64::MAX,
            -f64::MAX,
        ];
        for (chunk, value) in bytes.as_chunks_mut::<8>().0.iter_mut().zip(values) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        let weights = load_progress_coeff_kpabs_q16_from_bytes(&bytes).unwrap();
        assert_eq!(&weights[..6], &[1, -1, 1, -1, i32::MAX, i32::MIN]);
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            bytes[..8].copy_from_slice(&value.to_le_bytes());
            assert!(load_progress_coeff_kpabs_q16_from_bytes(&bytes).is_err());
        }
        assert!(load_progress_coeff_kpabs_q16_from_bytes(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn yo_thresholds_match_bulletou_scalar_progress_boundaries() {
        // 256 段階に量子化してから整数値 * N / 256 で振り分けるモデル形式の境界。
        // バケット数ごとの logit 閾値に対して upper_bound でバケットを選ぶ。
        let bullet: Vec<i64> = (1..256)
            .map(|i| {
                let p = i as f64 / 256.0;
                (p.ln() - (1.0 - p).ln()).mul_add(65536.0, 0.0).round() as i64
            })
            .collect();
        for n in [2, 4, 8, 16, 32, 64] {
            for &threshold in &bullet {
                for sum in [threshold - 1, threshold, threshold + 1] {
                    let value = bullet.partition_point(|&t| t <= sum);
                    assert_eq!(
                        progress_q16_sum_to_bucket(sum, n),
                        value * n / 256,
                        "N={n} sum={sum}"
                    );
                }
            }
            for k in 1..n {
                let t = bullet[k * 256 / n - 1];
                assert_eq!(progress_q16_sum_to_bucket(t - 1, n), k - 1);
                assert_eq!(progress_q16_sum_to_bucket(t, n), k);
                assert_eq!(progress_q16_sum_to_bucket(t + 1, n), k);
            }
        }
    }

    #[test]
    fn board_and_hand_sum_matches_yo_piece_lists_and_color_symmetry() {
        let weights: Vec<i32> = (0..SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS)
            .map(|i| (i as i32 * 7919) % 65537 - 32768)
            .collect();
        let mut sums = Vec::new();
        for sfen in [
            "4k4/9/9/9/9/9/2P6/9/4K4 b r 1",
            "4k4/9/6p2/9/9/9/9/9/4K4 w R 1",
        ] {
            let mut pos = Position::new();
            pos.set_sfen(sfen).unwrap();
            let bk = pos.king_square(Color::Black).index() * FE_OLD_END;
            let wk = pos.king_square(Color::White).inverse().index() * FE_OLD_END;
            // 両視点の BonaPiece リストを走査して Q16 係数を合算する。
            let reference: i64 = pos
                .piece_list()
                .piece_list_fb()
                .iter()
                .zip(pos.piece_list().piece_list_fw())
                .take(38)
                .filter(|(b, w)| **b != BonaPiece::ZERO && **w != BonaPiece::ZERO)
                .map(|(b, w)| {
                    i64::from(weights[bk + b.value() as usize])
                        + i64::from(weights[wk + w.value() as usize])
                })
                .sum();
            let actual = compute_progresskpabs_q16_sum(&pos, &weights);
            assert_eq!(actual, reference);
            assert_eq!(actual, reference_board_sums(&pos, &weights).iter().sum::<i64>());
            sums.push(actual);
        }
        assert_eq!(sums[0], sums[1]);
    }
}

#[cfg(all(test, feature = "layerstack-arch"))]
mod incremental_tests {
    use super::*;
    use crate::movegen::{MoveList, generate_legal_all};
    use crate::nnue::accumulator_layer_stacks::{
        AccumulatorStackLayerStacks, StackEntryLayerStacks,
    };
    use crate::nnue::network::{
        LayerStackBucketMode, configure_layer_stack_routing, layer_stack_routing_test_guard,
        reset_layer_stack_progress_buckets,
    };
    use rand::{Rng, SeedableRng};
    use rand_xoshiro::Xoshiro256PlusPlus;

    fn check<const N: usize>(
        pos: &Position,
        stack: &mut AccumulatorStackLayerStacks<N>,
        weights: &[i32],
    ) {
        let expected = reference_board_sums(pos, weights);
        let sum = expected.iter().sum();
        assert_eq!(compute_progresskpabs_q16_sum(pos, weights), sum);
        for count in [2, 4, 9, 16] {
            configure_layer_stack_routing(
                LayerStackBucketMode::ProgressKPAbsQ16,
                count,
                Some(count),
            )
            .unwrap();
            assert_eq!(
                stack.ensure_progress_q16_bucket(pos, count),
                progress_q16_sum_to_bucket(sum, count)
            );
            assert_eq!(stack.current().progress_q16, expected, "{}", pos.to_sfen());
            assert_eq!(stack.current().progress_q16_valid, 3);
        }
    }

    #[test]
    fn random_incremental_halves_match_board_scan() {
        let _guard = layer_stack_routing_test_guard();
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x0051_3136_4c46);
        let weights: Vec<i32> = (0..SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS)
            .map(|i| match i % 97 {
                0 => i32::MIN,
                1 => i32::MAX,
                _ => rng.random_range(-20000..=20000),
            })
            .collect();
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbsQ16, 16, Some(16))
            .unwrap();
        set_layer_stack_progress_kpabs_q16_weights(weights.clone().into_boxed_slice()).unwrap();
        let mut eager = AccumulatorStackLayerStacks::<32>::new();
        let mut lazy = AccumulatorStackLayerStacks::<32>::new();
        let mut counts = [0usize; 6]; // 手数・先手玉・後手玉・打ち・成り・捕獲
        let mut nulls = 0;
        for game in 0..200 {
            let mut pos = Position::new();
            pos.set_sfen(if game % 2 == 0 {
                crate::position::SFEN_HIRATE
            } else {
                "4k4/9/6p2/9/9/9/2P6/9/4K4 b RBGSNLrbgsnl 1"
            })
            .unwrap();
            eager.reset();
            lazy.reset();
            check(&pos, &mut eager, &weights);
            check(&pos, &mut lazy, &weights);
            let mut played = Vec::new();
            for ply in 0..200 {
                if !pos.in_check() && ply % 11 == 0 {
                    pos.do_null_move();
                    eager.push();
                    lazy.push();
                    check(&pos, &mut eager, &weights);
                    check(&pos, &mut lazy, &weights);
                    pos.undo_null_move();
                    eager.pop();
                    lazy.pop();
                    nulls += 1;
                }
                let mut moves = MoveList::new();
                generate_legal_all(&pos, &mut moves);
                if moves.is_empty() {
                    break;
                }
                let mv = moves.at(rng.random_range(0..moves.len()));
                counts[0] += 1;
                counts[3] += usize::from(mv.is_drop());
                counts[4] += usize::from(mv.is_promotion());
                counts[5] += usize::from(!pos.piece_on(mv.to()).is_none());
                let dirty = pos.do_move(mv, pos.gives_check(mv));
                for p in 0..2 {
                    counts[p + 1] += usize::from(dirty.king_moved[p]);
                }
                eager.push();
                lazy.push();
                eager.current_mut().dirty_piece = dirty;
                lazy.current_mut().dirty_piece = dirty;
                played.push(mv);
                check(&pos, &mut eager, &weights);
                // 8 手の上限内外と、未評価の祖先を経由する経路を交互に通す。
                if ply % [1, 8, 9, 17][game % 4] == 0 {
                    check(&pos, &mut lazy, &weights);
                }
                if ply % 7 == 6 {
                    pos.undo_move(played.pop().unwrap());
                    eager.pop();
                    lazy.pop();
                    check(&pos, &mut eager, &weights);
                }
            }
            for mv in played.into_iter().rev() {
                pos.undo_move(mv);
                eager.pop();
                lazy.pop();
                check(&pos, &mut eager, &weights);
                check(&pos, &mut lazy, &weights);
            }
        }
        assert!(counts[0] >= 30000, "{counts:?}");
        assert!(counts.iter().all(|&n| n > 0));
        assert!(nulls > 0);
        eprintln!(
            "Q16 cases={counts:?}, null={nulls}, entry1536={} bytes",
            size_of::<StackEntryLayerStacks<1536>>()
        );
        reset_layer_stack_progress_kpabs_q16_weights();
        reset_layer_stack_progress_buckets();
    }

    #[test]
    fn snapshot_survives_reload_and_reset_refreshes_it() {
        let _guard = layer_stack_routing_test_guard();
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbsQ16, 4, Some(4)).unwrap();
        let mut pos = Position::new();
        pos.set_hirate();
        let old = vec![i32::MAX; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS];
        let new = vec![i32::MIN; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS];
        set_layer_stack_progress_kpabs_q16_weights(old.clone().into_boxed_slice()).unwrap();
        let mut stack = AccumulatorStackLayerStacks::<32>::new();
        check(&pos, &mut stack, &old);
        set_layer_stack_progress_kpabs_q16_weights(new.clone().into_boxed_slice()).unwrap();
        reset_layer_stack_progress_kpabs_q16_weights();
        // 未計算の子でも取得済みの係数が生存し、祖先と同じ世代を使う。
        stack.push();
        check(&pos, &mut stack, &old);
        set_layer_stack_progress_kpabs_q16_weights(new.clone().into_boxed_slice()).unwrap();
        stack.reset();
        check(&pos, &mut stack, &new);
        reset_layer_stack_progress_kpabs_q16_weights();
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbsQ16, 1, Some(1)).unwrap();
        stack.reset();
        assert_eq!(stack.ensure_progress_q16_bucket(&pos, 1), 0);
        reset_layer_stack_progress_buckets();
    }

    #[test]
    fn walk_bound_and_king_refresh_are_per_perspective() {
        let _guard = layer_stack_routing_test_guard();
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbsQ16, 4, Some(4)).unwrap();
        let weights: Vec<i32> = (0..SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS)
            .map(|i| (i as i32 * 7919) % 65537 - 32768)
            .collect();
        set_layer_stack_progress_kpabs_q16_weights(weights.clone().into_boxed_slice()).unwrap();
        let mut pos = Position::new();
        pos.set_hirate();
        let mut stack = AccumulatorStackLayerStacks::<32>::new();
        for depth in [8, 9] {
            stack.reset();
            check(&pos, &mut stack, &weights);
            for _ in 0..depth {
                pos.do_null_move();
                stack.push();
            }
            check(&pos, &mut stack, &weights);
            for index in 1..depth {
                assert_eq!(
                    stack.entry_at(index).progress_q16_valid,
                    if depth == 8 { 3 } else { 0 }
                );
            }
            for _ in 0..depth {
                pos.undo_null_move();
            }
        }
        stack.reset();
        check(&pos, &mut stack, &weights);
        for usi in ["5i6h", "null", "6h5i", "null"] {
            if usi == "null" {
                pos.do_null_move();
                stack.push();
            } else {
                let mv = pos.to_move(crate::types::Move::from_usi(usi).unwrap()).unwrap();
                assert!(pos.is_legal(mv));
                let dirty = pos.do_move(mv, pos.gives_check(mv));
                stack.push();
                stack.current_mut().dirty_piece = dirty;
            }
        }
        check(&pos, &mut stack, &weights);
        // 往復した玉も再計算が必要。もう一方の視点だけ祖先から差分を伝播する。
        for index in 1..4 {
            assert_eq!(stack.entry_at(index).progress_q16_valid, 2);
        }
        stack.push();
        stack.current_mut().previous = Some(usize::MAX);
        check(&pos, &mut stack, &weights);
        reset_layer_stack_progress_kpabs_q16_weights();
        reset_layer_stack_progress_buckets();
    }
}
