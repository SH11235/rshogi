//! SkillLevel（手加減）統合テスト

use crate::position::Position;
use crate::search::engine::{Search, SearchInfo};
use crate::search::{LimitsType, SkillOptions};

/// SearchWorkerは大きなスタックを使うため 64MB 確保
const STACK_SIZE: usize = 64 * 1024 * 1024;

#[test]
fn skill_forces_multipv_to_four() {
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(|| {
            let mut search = Search::new(16);
            search.set_skill_options(SkillOptions {
                skill_level: 0, // Skill有効
                ..Default::default()
            });

            let mut pos = Position::new();
            pos.set_hirate();

            let limits = LimitsType {
                depth: 1,
                multi_pv: 1, // Skillにより強制的に >=4 になるはず
                ..Default::default()
            };

            let mut multipv = Vec::new();
            search.go(
                &mut pos,
                limits,
                Some(|info: &SearchInfo| {
                    if info.depth == 1 {
                        multipv.push(info.multi_pv);
                    }
                }),
            );

            let max_multipv = multipv.into_iter().max().unwrap_or(0);
            assert!(max_multipv >= 4, "Skill有効時はMultiPVが最低4まで引き上げられるはず");
        })
        .unwrap()
        .join()
        .unwrap();
}
