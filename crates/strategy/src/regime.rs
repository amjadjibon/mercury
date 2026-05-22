//! Two-state HMM regime filter for trend vs mean-reversion detection.
//!
//! The observation is return persistence: consecutive returns with the same
//! sign support the trending state, while sign flips support mean reversion.

/// Market regime inferred by [`HmmFilter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regime {
    Trending,
    MeanReverting,
}

/// Configurable parameters for the two-state HMM.
#[derive(Debug, Clone, Copy)]
pub struct HmmConfig {
    /// Probability that the hidden state remains unchanged at each step.
    pub stay_probability: f64,
    /// Gaussian emission mean for the trending state's persistence score.
    pub trend_mean: f64,
    /// Gaussian emission mean for the mean-reverting state's persistence score.
    pub mean_revert_mean: f64,
    /// Shared Gaussian emission standard deviation.
    pub emission_sigma: f64,
    /// Minimum state probability required before exposing a confident regime.
    pub confidence_threshold: f64,
}

impl Default for HmmConfig {
    fn default() -> Self {
        Self {
            stay_probability: 0.95,
            trend_mean: 0.75,
            mean_revert_mean: -0.75,
            emission_sigma: 0.55,
            confidence_threshold: 0.60,
        }
    }
}

/// Online forward filter for a 2-state hidden Markov model.
#[derive(Debug, Clone)]
pub struct HmmFilter {
    config: HmmConfig,
    trend_prob: f64,
    mean_revert_prob: f64,
    prev_price: Option<f64>,
    prev_return: Option<f64>,
}

impl HmmFilter {
    pub fn new(config: HmmConfig) -> Self {
        assert!(
            config.stay_probability > 0.0 && config.stay_probability < 1.0,
            "stay_probability must be in (0, 1)"
        );
        assert!(
            config.emission_sigma > 0.0,
            "emission_sigma must be positive"
        );
        assert!(
            config.confidence_threshold >= 0.5 && config.confidence_threshold <= 1.0,
            "confidence_threshold must be in [0.5, 1.0]"
        );

        Self {
            config,
            trend_prob: 0.5,
            mean_revert_prob: 0.5,
            prev_price: None,
            prev_return: None,
        }
    }

    /// Update from a mid-price tick and return the confident regime, if ready.
    pub fn update_price(&mut self, price: f64) -> Option<Regime> {
        if !price.is_finite() || price <= 0.0 {
            return self.regime();
        }

        let Some(prev_price) = self.prev_price.replace(price) else {
            return None;
        };
        let ret = (price / prev_price).ln();
        self.update_return(ret)
    }

    /// Update directly from a log return and return the confident regime, if ready.
    pub fn update_return(&mut self, ret: f64) -> Option<Regime> {
        if !ret.is_finite() {
            return self.regime();
        }

        let Some(prev_ret) = self.prev_return.replace(ret) else {
            return None;
        };

        let persistence = if ret == 0.0 || prev_ret == 0.0 {
            0.0
        } else {
            (ret.signum() * prev_ret.signum()).clamp(-1.0, 1.0)
        };
        self.observe(persistence);
        self.regime()
    }

    pub fn regime(&self) -> Option<Regime> {
        if self.trend_prob >= self.config.confidence_threshold {
            Some(Regime::Trending)
        } else if self.mean_revert_prob >= self.config.confidence_threshold {
            Some(Regime::MeanReverting)
        } else {
            None
        }
    }

    pub fn trend_probability(&self) -> f64 {
        self.trend_prob
    }

    pub fn mean_revert_probability(&self) -> f64 {
        self.mean_revert_prob
    }

    pub fn reset(&mut self) {
        self.trend_prob = 0.5;
        self.mean_revert_prob = 0.5;
        self.prev_price = None;
        self.prev_return = None;
    }

    fn observe(&mut self, observation: f64) {
        let switch_probability = 1.0 - self.config.stay_probability;
        let prior_trend = self.trend_prob * self.config.stay_probability
            + self.mean_revert_prob * switch_probability;
        let prior_mean_revert = self.mean_revert_prob * self.config.stay_probability
            + self.trend_prob * switch_probability;

        let trend_likelihood = gaussian_likelihood(
            observation,
            self.config.trend_mean,
            self.config.emission_sigma,
        );
        let mean_revert_likelihood = gaussian_likelihood(
            observation,
            self.config.mean_revert_mean,
            self.config.emission_sigma,
        );

        let trend = prior_trend * trend_likelihood;
        let mean_revert = prior_mean_revert * mean_revert_likelihood;
        let norm = trend + mean_revert;

        if norm > 0.0 && norm.is_finite() {
            self.trend_prob = trend / norm;
            self.mean_revert_prob = mean_revert / norm;
        }
    }
}

impl Default for HmmFilter {
    fn default() -> Self {
        Self::new(HmmConfig::default())
    }
}

/// Strategy-level regime configuration.
///
/// Consumed by `MarketMaker` (and other strategies) to scale spread width
/// based on the current inferred market regime.
#[derive(Debug, Clone, Copy)]
pub struct RegimeConfig {
    pub hmm: HmmConfig,
    /// Spread multiplier when regime is `Trending` (default 2.0 — widen spread).
    pub trending_spread_mult: f64,
    /// Spread multiplier when regime is `MeanReverting` (default 0.75 — tighten spread).
    pub mean_revert_spread_mult: f64,
}

impl Default for RegimeConfig {
    fn default() -> Self {
        Self {
            hmm: HmmConfig::default(),
            trending_spread_mult: 2.0,
            mean_revert_spread_mult: 0.75,
        }
    }
}

fn gaussian_likelihood(x: f64, mean: f64, sigma: f64) -> f64 {
    let z = (x - mean) / sigma;
    (-0.5 * z * z).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_same_direction_returns_detect_trending() {
        let mut filter = HmmFilter::default();

        for _ in 0..6 {
            filter.update_return(0.001);
        }

        assert_eq!(filter.regime(), Some(Regime::Trending));
        assert!(filter.trend_probability() > filter.mean_revert_probability());
    }

    #[test]
    fn alternating_returns_detect_mean_reversion() {
        let mut filter = HmmFilter::default();
        let returns = [0.001, -0.001, 0.001, -0.001, 0.001, -0.001];

        for ret in returns {
            filter.update_return(ret);
        }

        assert_eq!(filter.regime(), Some(Regime::MeanReverting));
        assert!(filter.mean_revert_probability() > filter.trend_probability());
    }

    #[test]
    fn price_updates_drive_filter_after_two_returns() {
        let mut filter = HmmFilter::default();

        assert_eq!(filter.update_price(100.0), None);
        assert_eq!(filter.update_price(101.0), None);
        let regime = filter.update_price(102.0);

        assert_eq!(regime, Some(Regime::Trending));
    }

    #[test]
    fn reset_returns_to_uncertain_state() {
        let mut filter = HmmFilter::default();
        for _ in 0..4 {
            filter.update_return(0.001);
        }
        assert!(filter.regime().is_some());

        filter.reset();

        assert_eq!(filter.regime(), None);
        assert_eq!(filter.trend_probability(), 0.5);
        assert_eq!(filter.mean_revert_probability(), 0.5);
    }
}
