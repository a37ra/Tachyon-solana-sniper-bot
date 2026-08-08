pub fn kelly_fraction(p: f64, b: f64) -> f64 {
    if b <= 0.0 || p <= 0.0 || p >= 1.0 {
        return 0.0;
    }

    let q = 1.0 - p;
    let f_star = p - (q / b);

    f_star.max(0.0)
}

pub fn fractional_kelly(f_star: f64, fraction: f64) -> f64 {
    if fraction <= 0.0 || fraction > 1.0 {
        return f_star;
    }

    f_star * fraction
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kelly_fraction() {
        let p = 0.6;
        let b = 2.0;
        let f = kelly_fraction(p, b);
        assert!((f - 0.4).abs() < 0.01);
    }

    #[test]
    fn test_kelly_fraction_edge_cases() {
        assert_eq!(kelly_fraction(0.0, 2.0), 0.0);
        assert_eq!(kelly_fraction(1.0, 2.0), 0.0);
        assert_eq!(kelly_fraction(0.5, 0.0), 0.0);
    }

    #[test]
    fn test_fractional_kelly() {
        let f_star = 0.2;
        let fraction = 0.25;
        let f = fractional_kelly(f_star, fraction);
        assert!((f - 0.05).abs() < 0.01);
    }
}
