//! Pure measurement math: percentiles, linear regression, averages.
//!
//! Percentiles use linear interpolation between order statistics (the numpy
//! default), which is what "p95" means in the spec's benchmark tables.

/// Linear-interpolated percentile of `p` (0..=100). Input need not be
/// sorted; `None` for an empty slice.
pub fn percentile(mut samples: Vec<f64>, p: f64) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let p = p.clamp(0.0, 100.0);
    samples.sort_by(|a, b| a.total_cmp(b));
    let rank = (p / 100.0) * (samples.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        return Some(samples[lo]);
    }
    let frac = rank - lo as f64;
    Some(samples[lo] * (1.0 - frac) + samples[hi] * frac)
}

pub fn mean(samples: &[f64]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    Some(samples.iter().sum::<f64>() / samples.len() as f64)
}

/// Least-squares line fit. `r2` is the coefficient of determination
/// (1.0 = perfectly linear, ~0 = noise).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineFit {
    pub slope: f64,
    pub intercept: f64,
    pub r2: f64,
}

pub fn linear_regression(xs: &[f64], ys: &[f64]) -> Option<LineFit> {
    if xs.len() != ys.len() || xs.len() < 2 {
        return None;
    }
    let mean_x = mean(xs)?;
    let mean_y = mean(ys)?;
    let mut sxx = 0.0;
    let mut sxy = 0.0;
    let mut syy = 0.0;
    for (&x, &y) in xs.iter().zip(ys) {
        let (dx, dy) = (x - mean_x, y - mean_y);
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    if sxx == 0.0 {
        return None;
    }
    let slope = sxy / sxx;
    let intercept = mean_y - slope * mean_x;
    let r2 = if syy == 0.0 {
        1.0
    } else {
        (sxy * sxy) / (sxx * syy)
    };
    Some(LineFit {
        slope,
        intercept,
        r2,
    })
}

/// Average cores from sysinfo process `cpu_usage()` percentages, where
/// 100 % == one logical core (values above 100 mean multiple threads).
pub fn average_cores(cpu_percent_samples: &[f32]) -> Option<f64> {
    mean(
        &cpu_percent_samples
            .iter()
            .map(|&v| v as f64 / 100.0)
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn percentile_interpolates_between_order_statistics() {
        let v: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        assert!(close(percentile(v.clone(), 50.0).unwrap(), 50.5));
        assert!(close(percentile(v.clone(), 95.0).unwrap(), 95.05));
        assert!(close(percentile(v.clone(), 99.0).unwrap(), 99.01));
        assert!(close(percentile(v.clone(), 0.0).unwrap(), 1.0));
        assert!(close(percentile(v.clone(), 100.0).unwrap(), 100.0));
    }

    #[test]
    fn percentile_handles_tiny_inputs_and_sorts() {
        assert!(close(percentile(vec![7.0], 99.0).unwrap(), 7.0));
        assert_eq!(percentile(Vec::new(), 50.0), None);
        // Unsorted input must be sorted first.
        assert!(close(
            percentile(vec![30.0, 10.0, 20.0], 50.0).unwrap(),
            20.0
        ));
        assert!(close(
            percentile(vec![30.0, 10.0, 20.0], 90.0).unwrap(),
            28.0
        ));
    }

    #[test]
    fn regression_fits_perfect_line() {
        let xs = vec![0.0, 1.0, 2.0, 3.0, 4.0];
        let ys: Vec<f64> = xs.iter().map(|x| 2.0 * x + 1.0).collect();
        let fit = linear_regression(&xs, &ys).unwrap();
        assert!(close(fit.slope, 2.0));
        assert!(close(fit.intercept, 1.0));
        assert!(close(fit.r2, 1.0));
    }

    #[test]
    fn regression_flat_series_has_zero_slope() {
        let xs = vec![0.0, 2.0, 4.0, 6.0];
        let ys = vec![10.0, 10.0, 10.0, 10.0];
        let fit = linear_regression(&xs, &ys).unwrap();
        assert!(close(fit.slope, 0.0));
        assert!(close(fit.r2, 1.0));
    }

    #[test]
    fn regression_needs_two_distinct_x() {
        assert!(linear_regression(&[1.0, 1.0], &[1.0, 2.0]).is_none());
        assert!(linear_regression(&[0.0], &[1.0]).is_none());
    }

    #[test]
    fn average_cores_converts_percent() {
        assert!(close(
            average_cores(&[100.0, 200.0, 0.0]).unwrap(),
            (1.0 + 2.0 + 0.0) / 3.0
        ));
        assert!(average_cores(&[]).is_none());
    }
}
