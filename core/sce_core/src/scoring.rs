use crate::analyzer::EPS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorridorDirection {
    Inside,
    TooLow,
    TooHigh,
}

pub fn corridor_deviation(value: f64, min: f64, max: f64) -> (f64, CorridorDirection) {
    let low = min.min(max);
    let high = min.max(max);

    if low <= value && value <= high {
        return (0.0, CorridorDirection::Inside);
    }

    let width = (high - low).max(EPS);

    if value < low {
        (
            ((low - value) / width).clamp(0.0, 1.0),
            CorridorDirection::TooLow,
        )
    } else {
        (
            ((value - high) / width).clamp(0.0, 1.0),
            CorridorDirection::TooHigh,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{corridor_deviation, CorridorDirection};

    #[test]
    fn corridor_bounds_are_inclusive() {
        assert_eq!(
            corridor_deviation(1.0, 1.0, 2.0),
            (0.0, CorridorDirection::Inside)
        );
        assert_eq!(
            corridor_deviation(2.0, 1.0, 2.0),
            (0.0, CorridorDirection::Inside)
        );
    }
}
