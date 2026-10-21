/// Summary statistics of a list of numbers.
#[derive(Debug, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub mean: f64,
    pub min: f64,
    pub max: f64,
}

pub fn summarize(values: &Vec<f64>) -> Option<Summary> {
    if values.len() == 0 {
        return None;
    }
    let mut total = 0.0;
    for i in 0..values.len() {
        total = total + values[i];
    }
    let mut min = values[0];
    let mut max = values[0];
    for v in values.iter() {
        if *v < min {
            min = *v;
        }
        if *v > max {
            max = *v;
        }
    }
    return Some(Summary { count: values.len(), mean: total / values.len() as f64, min: min, max: max });
}

/// Parses comma- or whitespace-separated numbers, skipping anything that isn't one.
pub fn parse_numbers(text: &str) -> Vec<f64> {
    let cleaned = text.replace(",", " ");
    let mut out = Vec::new();
    for word in cleaned.split_whitespace() {
        match word.parse::<f64>() {
            Ok(n) => out.push(n),
            Err(_) => {}
        }
    }
    out
}

/// Labels a value relative to a summary.
pub fn label(value: f64, summary: &Summary) -> String {
    if value > summary.mean {
        if value == summary.max {
            return "max".to_string();
        } else {
            return "above".to_string();
        }
    }
    if value == summary.min { "min".to_string() } else { "below".to_string() }
}
