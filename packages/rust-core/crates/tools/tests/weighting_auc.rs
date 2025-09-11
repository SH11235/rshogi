use tools::stats::roc_auc_weighted;

#[test]
fn endgame_heavy_changes_weighted_auc() {
    // Construct toy predictions/labels: endgame subset has better separation
    // predictions are probabilities for positive class
    let probs: Vec<f32> = vec![0.9, 0.8, 0.2, 0.1, 0.7, 0.6, 0.4, 0.3];
    let labels: Vec<f32> = vec![1.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0];
    // First 4 = endgame, last 4 = non-endgame
    let w_base: Vec<f32> = vec![1.0; 8];
    let w_endgame_heavy: Vec<f32> = vec![2.0, 2.0, 2.0, 2.0, 1.0, 1.0, 1.0, 1.0];

    let auc_base = roc_auc_weighted(&probs, &labels, &w_base).unwrap();
    let auc_heavy = roc_auc_weighted(&probs, &labels, &w_endgame_heavy).unwrap();

    assert!(
        (auc_base - auc_heavy).abs() > 1e-6,
        "AUC should change with endgame-heavy weights"
    );
}
