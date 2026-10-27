//! Great-circle helpers.

/// Mean earth radius in nautical miles.
const EARTH_RADIUS_NM: f64 = 3440.065;

/// Great-circle distance in nautical miles between two lat/lon points (degrees).
pub fn distance_nm(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_NM * a.sqrt().min(1.0).asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_degree_of_latitude_is_sixty_nm() {
        let d = distance_nm(10.0, 20.0, 11.0, 20.0);
        assert!((d - 60.04).abs() < 0.1, "{d}");
    }

    #[test]
    fn known_city_pair() {
        // KSEA -> KPDX is about 112 nm.
        let d = distance_nm(47.4490, -122.3093, 45.5887, -122.5975);
        assert!((d - 112.0).abs() < 2.0, "{d}");
    }

    #[test]
    fn antimeridian() {
        let d = distance_nm(0.0, 179.5, 0.0, -179.5);
        assert!((d - 60.0).abs() < 0.2, "{d}");
    }
}
