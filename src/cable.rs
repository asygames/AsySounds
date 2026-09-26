//! Explicit virtual-cable discovery. Never confuses a playback endpoint with
//! the corresponding capture endpoint or changes Windows defaults.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CableRoute {
    pub playback: String,
    pub capture: Option<String>,
}

/// Windows VB-CABLE endpoints typically use CABLE Input/CABLE Output or
/// CABLE-A Input/CABLE-A Output. Stay conservative: unknown devices are not
/// silently treated as a microphone route.
fn cable_variant(name: &str, side: &str) -> Option<String> {
    let name = name.trim().to_ascii_lowercase();
    let (prefix, _) = name.split_once(side)?;
    let suffix = prefix.strip_prefix("cable")?;
    let suffix = suffix.trim_matches(|c: char| c.is_ascii_whitespace() || c == '-' || c == '_');
    match suffix {
        "" => Some("default".to_owned()),
        "a" | "b" | "c" | "d" => Some(suffix.to_owned()),
        _ => None,
    }
}

pub fn find_cable_routes(inputs: &[String], outputs: &[String]) -> Vec<CableRoute> {
    outputs
        .iter()
        .filter_map(|playback| {
            let variant = cable_variant(playback, "input")?;
            let capture = inputs
                .iter()
                .find(|name| cable_variant(name, "output").as_deref() == Some(&variant))
                .cloned();
            Some(CableRoute {
                playback: playback.clone(),
                capture,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_default_and_lettered_cables_without_cross_wiring() {
        let inputs = vec![
            "CABLE Output (VB-Audio Virtual Cable)".to_owned(),
            "CABLE-A Output (VB-Audio Cable A)".to_owned(),
            "Microphone (HyperX)".to_owned(),
        ];
        let outputs = vec![
            "Headphones (HyperX)".to_owned(),
            "CABLE-A Input (VB-Audio Cable A)".to_owned(),
            "CABLE Input (VB-Audio Virtual Cable)".to_owned(),
        ];
        let routes = find_cable_routes(&inputs, &outputs);
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].capture.as_deref(), Some(inputs[1].as_str()));
        assert_eq!(routes[1].capture.as_deref(), Some(inputs[0].as_str()));
    }

    #[test]
    fn missing_capture_is_not_a_working_route() {
        let routes = find_cable_routes(&[], &["CABLE Input (VB-Audio Virtual Cable)".into()]);
        assert_eq!(routes.len(), 1);
        assert!(routes[0].capture.is_none());
    }

    #[test]
    fn hardware_and_misleading_names_are_not_cables() {
        let outputs = vec![
            "Speakers (USB cable)".into(),
            "Cable Microphone".into(),
            "Headphones".into(),
        ];
        assert!(find_cable_routes(&[], &outputs).is_empty());
    }
}
