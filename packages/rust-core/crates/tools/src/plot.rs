#[cfg(feature = "plots")]
pub fn plot_calibration_png<P: AsRef<std::path::Path>>(
    path: P,
    bins: &[(i32, i32, f32, f64, f64)], // (left, right, center, mean_pred, mean_label)
) -> Result<(), Box<dyn std::error::Error>> {
    use plotters::prelude::*;
    let root = BitMapBackend::new(path.as_ref(), (640, 480)).into_drawing_area();
    root.fill(&WHITE)?;
    let x_min = bins.first().map(|b| b.0).unwrap_or(-1200) as f32;
    let x_max = bins.last().map(|b| b.1).unwrap_or(1200) as f32;
    let mut chart = ChartBuilder::on(&root)
        .margin(20)
        .caption("Calibration", ("sans-serif", 22))
        .x_label_area_size(45)
        .y_label_area_size(45)
        .build_cartesian_2d(x_min..x_max, 0.0f32..1.0f32)?;
    chart.configure_mesh().x_desc("CP (clipped)").y_desc("Probability").draw()?;
    // diagonal y=x scaled to [0,1] over x range; use cp normalized to 0..1 reference line is not meaningful.
    // Instead draw y=0..1 reference lines.
    let pred_series: Vec<(f32, f32)> = bins.iter().map(|b| (b.2, b.3 as f32)).collect();
    let label_series: Vec<(f32, f32)> = bins.iter().map(|b| (b.2, b.4 as f32)).collect();
    chart
        .draw_series(LineSeries::new(pred_series, &BLUE))?
        .label("mean_pred")
        .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], BLUE.filled()));
    chart
        .draw_series(LineSeries::new(label_series, &RED))?
        .label("mean_label")
        .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], RED.filled()));
    chart
        .configure_series_labels()
        .background_style(&WHITE.mix(0.8))
        .border_style(&BLACK)
        .draw()?;
    root.present()?;
    Ok(())
}

#[cfg(not(feature = "plots"))]
pub fn plot_calibration_png<P: AsRef<std::path::Path>>(
    _path: P,
    _bins: &[(i32, i32, f32, f64, f64)],
) -> Result<(), Box<dyn std::error::Error>> {
    Err("plots feature is not enabled".into())
}
