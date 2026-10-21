/// Summary statistics of a list of numbers.
#[derive(Debug, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub mean: f64,
    pub min: f64,
    pub max: f64,
}

pub fn summarize(values: &[f64]) -> Option<Summary> {
    if values.is_empty() {
        return None;
    }
    let total: f64 = values.iter().sum();
    let min = values.iter().copied().fold(values[0], f64::min);
    let max = values.iter().copied().fold(values[0], f64::max);
    Some(Summary { count: values.len(), mean: total / values.len() as f64, min, max })
}

/// Parses comma- or whitespace-separated numbers, skipping anything that isn't one.
pub fn parse_numbers(text: &str) -> Vec<f64> {
    text.replace(',', " ").split_whitespace().filter_map(|word| word.parse::<f64>().ok()).collect()
}

/// Labels a value relative to a summary.
pub fn label(value: f64, summary: &Summary) -> String {
    if value > summary.mean {
        return if value == summary.max { "max" } else { "above" }.to_string();
    }
    if value == summary.min { "min".to_string() } else { "below".to_string() }
}
