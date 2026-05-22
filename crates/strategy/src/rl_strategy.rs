//! Deep Q-Network (DQN) quote-placement strategy.
//!
//! The agent observes a 7-dimensional market state (6 market features + HMM regime
//! probability) and selects one of 9 discrete actions encoding (spread_width,
//! inventory_skew) for the next posted quotes.
//!
//! ## Action space  (3 spread widths × 3 skew levels = 9 actions)
//!
//! | idx | spread × base | skew (σ units) |
//! |-----|---------------|----------------|
//! |  0  |     1.0×      |     −0.5       |
//! |  1  |     1.0×      |      0.0       |
//! |  2  |     1.0×      |     +0.5       |
//! |  3  |     1.5×      |     −0.5       |
//! |  4  |     1.5×      |      0.0       |
//! |  5  |     1.5×      |     +0.5       |
//! |  6  |     2.5×      |     −0.5       |
//! |  7  |     2.5×      |      0.0       |
//! |  8  |     2.5×      |     +0.5       |
//!
//! ## Reward
//! `reward = Δ(unrealised PnL) − α · inventory²`
//!
//! ## Q-network
//! 2-hidden-layer MLP (7 → 24 → 12 → 9), trained online via experience replay
//! (2 000-step ring buffer, batch SGD, target-network sync every 100 train steps).

use crate::regime::{HmmConfig, HmmFilter};
use crate::traits::Strategy;
use mercury_core::{Fill, FixedPoint, OrderBook, OrderType, Quantity, Side, Signal, StrategyId, Trade};
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal_macros::dec;

// ── Network dimensions ────────────────────────────────────────────────────────

const STATE_DIM: usize = 7;
const HIDDEN1: usize = 24;
const HIDDEN2: usize = 12;
const N_ACTIONS: usize = 9;
const REPLAY_CAP: usize = 2_000;
const BATCH_SIZE: usize = 32;
const MIN_REPLAY: usize = 64;
const TARGET_SYNC: usize = 100;
const TRAIN_EVERY: u64 = 4;

// ── Action codec ──────────────────────────────────────────────────────────────

// action = spread_idx * 3 + skew_idx
const SPREAD_MULTS: [f32; 3] = [1.0, 1.5, 2.5];
const SKEWS: [f32; 3] = [-0.5, 0.0, 0.5]; // σ units

#[inline]
fn decode_action(a: usize) -> (f32, f32) {
    (SPREAD_MULTS[a / 3], SKEWS[a % 3])
}

fn argmax(v: &[f32]) -> usize {
    v.iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .unwrap_or(0)
}

// ── xorshift64 RNG ────────────────────────────────────────────────────────────

fn xorshift64(s: &mut u64) -> u64 {
    let mut x = *s;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *s = x;
    x
}

#[inline]
fn rand_f32(s: &mut u64) -> f32 {
    (xorshift64(s) >> 11) as f32 * (1.0f32 / (1u64 << 53) as f32)
}

#[inline]
fn rand_usize(s: &mut u64, n: usize) -> usize {
    (xorshift64(s) % n as u64) as usize
}

// He-init uniform range for a weight from fan_in inputs.
fn he_init(fan_in: usize, rng: &mut u64) -> f32 {
    let r = (2.0f32 / fan_in as f32).sqrt();
    rand_f32(rng) * 2.0 * r - r
}

// ── 2-layer MLP Q-network ─────────────────────────────────────────────────────

#[derive(Clone)]
struct MlpQNet {
    w1: Vec<Vec<f32>>, // [HIDDEN1][STATE_DIM]
    b1: Vec<f32>,      // [HIDDEN1]
    w2: Vec<Vec<f32>>, // [HIDDEN2][HIDDEN1]
    b2: Vec<f32>,      // [HIDDEN2]
    w3: Vec<Vec<f32>>, // [N_ACTIONS][HIDDEN2]
    b3: Vec<f32>,      // [N_ACTIONS]
}

impl MlpQNet {
    fn new(rng: &mut u64) -> Self {
        Self {
            w1: (0..HIDDEN1)
                .map(|_| (0..STATE_DIM).map(|_| he_init(STATE_DIM, rng)).collect())
                .collect(),
            b1: vec![0.0; HIDDEN1],
            w2: (0..HIDDEN2)
                .map(|_| (0..HIDDEN1).map(|_| he_init(HIDDEN1, rng)).collect())
                .collect(),
            b2: vec![0.0; HIDDEN2],
            w3: (0..N_ACTIONS)
                .map(|_| (0..HIDDEN2).map(|_| he_init(HIDDEN2, rng)).collect())
                .collect(),
            b3: vec![0.0; N_ACTIONS],
        }
    }

    /// Full forward pass returning all intermediate activations for backprop.
    fn forward_full(
        &self,
        s: &[f32],
    ) -> (Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>) {
        let mut z1 = vec![0.0f32; HIDDEN1];
        for i in 0..HIDDEN1 {
            z1[i] = self.b1[i];
            for j in 0..STATE_DIM {
                z1[i] += self.w1[i][j] * s[j];
            }
        }
        let h1: Vec<f32> = z1.iter().map(|&x| x.max(0.0)).collect();

        let mut z2 = vec![0.0f32; HIDDEN2];
        for i in 0..HIDDEN2 {
            z2[i] = self.b2[i];
            for j in 0..HIDDEN1 {
                z2[i] += self.w2[i][j] * h1[j];
            }
        }
        let h2: Vec<f32> = z2.iter().map(|&x| x.max(0.0)).collect();

        let mut q = vec![0.0f32; N_ACTIONS];
        for i in 0..N_ACTIONS {
            q[i] = self.b3[i];
            for j in 0..HIDDEN2 {
                q[i] += self.w3[i][j] * h2[j];
            }
        }
        (z1, h1, z2, h2, q)
    }

    fn forward(&self, s: &[f32]) -> Vec<f32> {
        self.forward_full(s).4
    }

    /// SGD update for a single (s, action, td_target) tuple.
    ///
    /// Only the Q-output for `action` has a non-zero loss gradient; all other
    /// output units contribute zero gradient and their weights are unchanged.
    fn update(&mut self, s: &[f32], action: usize, td_target: f32, lr: f32) {
        let (z1, h1, z2, h2, q) = self.forward_full(s);

        let err = q[action] - td_target; // dL/dq[action] (factor of 2 absorbed into lr)

        // Layer 3: compute dh2 from original W3 before updating weights.
        let dh2_raw: Vec<f32> = (0..HIDDEN2).map(|j| err * self.w3[action][j]).collect();
        for j in 0..HIDDEN2 {
            self.w3[action][j] -= lr * err * h2[j];
        }
        self.b3[action] -= lr * err;

        // ReLU gate at h2.
        let dh2: Vec<f32> = dh2_raw
            .iter()
            .zip(z2.iter())
            .map(|(&d, &z)| if z > 0.0 { d } else { 0.0 })
            .collect();

        // Layer 2: compute dh1 from original W2 before updating weights.
        let dh1_raw: Vec<f32> = (0..HIDDEN1)
            .map(|k| (0..HIDDEN2).map(|i| dh2[i] * self.w2[i][k]).sum::<f32>())
            .collect();
        for i in 0..HIDDEN2 {
            for k in 0..HIDDEN1 {
                self.w2[i][k] -= lr * dh2[i] * h1[k];
            }
            self.b2[i] -= lr * dh2[i];
        }

        // ReLU gate at h1.
        let dh1: Vec<f32> = dh1_raw
            .iter()
            .zip(z1.iter())
            .map(|(&d, &z)| if z > 0.0 { d } else { 0.0 })
            .collect();

        // Layer 1.
        for i in 0..HIDDEN1 {
            for k in 0..STATE_DIM {
                self.w1[i][k] -= lr * dh1[i] * s[k];
            }
            self.b1[i] -= lr * dh1[i];
        }
    }
}

// ── Experience replay ─────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct Experience {
    state: [f32; STATE_DIM],
    action: usize,
    reward: f32,
    next_state: [f32; STATE_DIM],
}

struct ReplayBuffer {
    buf: Vec<Experience>,
    head: usize,
}

impl ReplayBuffer {
    fn new() -> Self {
        Self {
            buf: Vec::with_capacity(REPLAY_CAP),
            head: 0,
        }
    }

    fn push(&mut self, exp: Experience) {
        if self.buf.len() < REPLAY_CAP {
            self.buf.push(exp);
        } else {
            self.buf[self.head] = exp;
            self.head = (self.head + 1) % REPLAY_CAP;
        }
    }

    fn len(&self) -> usize {
        self.buf.len()
    }

    fn sample(&self, rng: &mut u64) -> &Experience {
        &self.buf[rand_usize(rng, self.buf.len())]
    }
}

// ── Hyperparameters ───────────────────────────────────────────────────────────

/// Hyperparameters for `RlQuotePlacementStrategy`.
#[derive(Debug, Clone)]
pub struct DqnConfig {
    /// Discount factor γ.
    pub gamma: f32,
    /// SGD learning rate.
    pub learning_rate: f32,
    /// Initial exploration probability ε.
    pub epsilon_start: f32,
    /// Minimum exploration probability after decay.
    pub epsilon_min: f32,
    /// Multiplicative ε-decay applied after each action selection.
    pub epsilon_decay: f32,
    /// Inventory penalty coefficient α in the reward function.
    pub inventory_penalty: f32,
}

impl Default for DqnConfig {
    fn default() -> Self {
        Self {
            gamma: 0.99,
            learning_rate: 1e-3,
            epsilon_start: 1.0,
            epsilon_min: 0.05,
            epsilon_decay: 0.9995,
            inventory_penalty: 0.01,
        }
    }
}

// ── Strategy ──────────────────────────────────────────────────────────────────

/// DQN market-making strategy: learns (spread, skew) placement via TD learning.
///
/// On each book update the agent:
/// 1. Computes a 6-feature state vector.
/// 2. Selects an action via ε-greedy policy over the online Q-network.
/// 3. Emits PostOnly limit signals for the chosen (spread, skew).
/// 4. Stores the previous transition in the replay buffer.
/// 5. Every `TRAIN_EVERY` ticks runs a mini-batch SGD step.
/// 6. Every `TARGET_SYNC` train steps copies the online network to the target.
pub struct RlQuotePlacementStrategy {
    // Market-making params
    order_size: Quantity,
    base_half_spread: Decimal,
    max_inventory: Quantity,

    // Agent internals
    config: DqnConfig,
    q_net: MlpQNet,
    target_net: MlpQNet,
    replay: ReplayBuffer,
    epsilon: f32,
    rng: u64,
    tick_count: u64,
    train_steps: usize,

    // Running state
    inventory: Quantity,
    prev_mid: Option<Decimal>,
    session_pnl: Decimal,
    variance_ema: f32,
    prev_state: Option<[f32; STATE_DIM]>,
    prev_action: Option<usize>,

    // Regime filter
    hmm: HmmFilter,
}

impl RlQuotePlacementStrategy {
    /// Create a new agent.
    ///
    /// - `base_spread_bps`: floor half-spread in basis points (e.g. 5 = 0.05%).
    /// - `max_inventory`: position at which `inv_norm` saturates to ±1.
    pub fn new(
        order_size: Quantity,
        base_spread_bps: u32,
        max_inventory: Quantity,
        config: DqnConfig,
    ) -> Self {
        let mut rng: u64 = 0xdeadbeef_cafebabe;
        let q_net = MlpQNet::new(&mut rng);
        let target_net = q_net.clone();

        Self {
            order_size,
            base_half_spread: Decimal::from(base_spread_bps) / dec!(20_000),
            max_inventory,
            epsilon: config.epsilon_start,
            config,
            q_net,
            target_net,
            replay: ReplayBuffer::new(),
            rng,
            tick_count: 0,
            train_steps: 0,
            inventory: Decimal::ZERO,
            prev_mid: None,
            session_pnl: Decimal::ZERO,
            variance_ema: 1e-6,
            prev_state: None,
            prev_action: None,
            hmm: HmmFilter::new(HmmConfig::default()),
        }
    }

    /// Override the RNG seed for reproducible experiments.
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.rng = seed;
        self
    }

    /// Current ε (exploration rate).
    pub fn epsilon(&self) -> f32 {
        self.epsilon
    }

    /// Total SGD training steps completed.
    pub fn train_steps(&self) -> usize {
        self.train_steps
    }

    /// Current replay buffer occupancy.
    pub fn replay_len(&self) -> usize {
        self.replay.len()
    }

    // ── State computation ─────────────────────────────────────────────────────

    fn compute_state(&mut self, book: &OrderBook, mid: Decimal) -> [f32; STATE_DIM] {
        let mid_f = mid.to_f32().unwrap_or(0.0);
        self.hmm.update_price(mid_f as f64);

        // Feature 0: normalised mid-price return (clamped to ±2%)
        let mid_return = if let Some(prev) = self.prev_mid {
            let prev_f = prev.to_f32().unwrap_or(mid_f);
            if prev_f > 0.0 {
                let r = (mid_f - prev_f) / prev_f;
                // Update EMA variance inline while we have the return.
                self.variance_ema = 0.095 * r * r + 0.905 * self.variance_ema;
                (r * 100.0).clamp(-2.0, 2.0)
            } else {
                0.0
            }
        } else {
            0.0
        };

        // Feature 1: bid-ask spread in bps, normalised to [0, 1] at 100 bps ceiling.
        let spread_norm = book
            .spread_bps()
            .map(|s| s.to_decimal().to_f32().unwrap_or(0.0))
            .unwrap_or(0.0)
            .min(100.0)
            / 100.0;

        // Feature 2: signed inventory fraction, clipped to [-1, 1].
        let inv_norm = (self.inventory / self.max_inventory.max(dec!(1)))
            .to_f32()
            .unwrap_or(0.0)
            .clamp(-1.0, 1.0);

        // Feature 3: volatility proxy — sqrt(EMA variance), normalised.
        let vol_norm = self.variance_ema.sqrt().min(0.02) * 50.0;

        // Feature 4: top-5 order-book imbalance ∈ [-1, 1].
        let obi = book.imbalance(5) as f32;

        // Feature 5: session PnL normalised by 100-order notional.
        let notional = (self.order_size * mid * dec!(100))
            .to_f32()
            .unwrap_or(1.0)
            .abs()
            .max(1.0);
        let pnl_norm = (self.session_pnl.to_f32().unwrap_or(0.0) / notional).clamp(-1.0, 1.0);

        // Feature 6: HMM trend probability centred at 0; 0.5 trending, -0.5 mean-reverting.
        let regime_feat = self.hmm.trend_probability() as f32 - 0.5;

        [mid_return, spread_norm, inv_norm, vol_norm, obi, pnl_norm, regime_feat]
    }

    // ── Reward ────────────────────────────────────────────────────────────────

    fn compute_reward(&self, mid: Decimal) -> f32 {
        let inv = self.inventory.to_f32().unwrap_or(0.0);
        let delta_pnl = if let Some(prev) = self.prev_mid {
            (mid - prev).to_f32().unwrap_or(0.0) * inv
        } else {
            0.0
        };
        let penalty = self.config.inventory_penalty * inv * inv;
        delta_pnl - penalty
    }

    // ── Training ──────────────────────────────────────────────────────────────

    fn train_step(&mut self) {
        if self.replay.len() < MIN_REPLAY {
            return;
        }
        let lr = self.config.learning_rate;
        let gamma = self.config.gamma;

        for _ in 0..BATCH_SIZE {
            // sample() borrows replay immutably; clone() releases the borrow.
            let exp = self.replay.sample(&mut self.rng).clone();
            let q_next = self.target_net.forward(&exp.next_state);
            let max_q = q_next.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let td_target = exp.reward + gamma * max_q;
            self.q_net.update(&exp.state, exp.action, td_target, lr);
        }

        self.train_steps += 1;
        if self.train_steps % TARGET_SYNC == 0 {
            self.target_net = self.q_net.clone();
        }
    }
}

impl Strategy for RlQuotePlacementStrategy {
    fn id(&self) -> StrategyId {
        StrategyId::RlQuotePlacement
    }

    fn on_book(&mut self, book: &OrderBook) -> Vec<Signal> {
        self.tick_count += 1;

        let Some(mid_fp) = book.mid_price() else {
            return vec![];
        };
        let mid = mid_fp.to_decimal();

        let state = self.compute_state(book, mid);

        // Store the (prev_state, prev_action, reward, state) transition.
        if let (Some(ps), Some(pa)) = (self.prev_state, self.prev_action) {
            let reward = self.compute_reward(mid);
            self.replay.push(Experience {
                state: ps,
                action: pa,
                reward,
                next_state: state,
            });

            if self.tick_count % TRAIN_EVERY == 0 {
                self.train_step();
            }
        }

        // ε-greedy action selection.
        let action = if rand_f32(&mut self.rng) < self.epsilon {
            rand_usize(&mut self.rng, N_ACTIONS)
        } else {
            let q = self.q_net.forward(&state);
            argmax(&q)
        };

        self.epsilon = (self.epsilon * self.config.epsilon_decay).max(self.config.epsilon_min);

        self.prev_state = Some(state);
        self.prev_action = Some(action);
        self.prev_mid = Some(mid);

        // Translate action to quote prices.
        let (spread_mult, skew_sigma) = decode_action(action);
        let vol = Decimal::from_f32(self.variance_ema.sqrt()).unwrap_or(dec!(0.001));
        let half_spread =
            (self.base_half_spread * Decimal::from_f32(spread_mult).unwrap_or(Decimal::ONE)) * mid;
        let skew_shift =
            mid * vol * Decimal::from_f32(skew_sigma).unwrap_or(Decimal::ZERO);

        let reservation = mid - skew_shift;
        let bid = reservation - half_spread;
        let ask = reservation + half_spread;

        if bid <= Decimal::ZERO || ask <= bid {
            return vec![];
        }

        vec![
            Signal {
                symbol: book.symbol,
                side: Side::Buy,
                order_type: OrderType::Limit,
                price: Some(FixedPoint::from_decimal(bid)),
                quantity: FixedPoint::from_decimal(self.order_size),
                strategy: self.id(),
                cancel_replace: true,
                time_in_force: mercury_core::TimeInForce::PostOnly,
            },
            Signal {
                symbol: book.symbol,
                side: Side::Sell,
                order_type: OrderType::Limit,
                price: Some(FixedPoint::from_decimal(ask)),
                quantity: FixedPoint::from_decimal(self.order_size),
                strategy: self.id(),
                cancel_replace: true,
                time_in_force: mercury_core::TimeInForce::PostOnly,
            },
        ]
    }

    fn on_trade(&mut self, _trade: &Trade) -> Vec<Signal> {
        vec![]
    }

    fn on_fill(&mut self, fill: &Fill) {
        match fill.side {
            Side::Buy => {
                self.inventory += fill.quantity;
                self.session_pnl -= fill.price * fill.quantity;
            }
            Side::Sell => {
                self.inventory -= fill.quantity;
                self.session_pnl += fill.price * fill.quantity;
            }
        }
    }

    fn reset(&mut self) {
        self.inventory = Decimal::ZERO;
        self.prev_mid = None;
        self.session_pnl = Decimal::ZERO;
        self.tick_count = 0;
        self.prev_state = None;
        self.prev_action = None;
        self.variance_ema = 1e-6;
        self.epsilon = self.config.epsilon_start;
        self.hmm.reset();
        // Q-network weights are intentionally preserved across resets so that
        // accumulated learning survives episode boundaries.
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{BookUpdate, Exchange, Level, Symbol};
    use rust_decimal_macros::dec;

    fn make_book(bid: Decimal, ask: Decimal) -> OrderBook {
        let mut book = OrderBook::new(Exchange::Binance, Symbol::new("BTCUSDT"));
        book.apply_update(&BookUpdate::from_slices(
            Exchange::Binance,
            Symbol::new("BTCUSDT"),
            &[Level::new(bid, dec!(1.0))],
            &[Level::new(ask, dec!(1.0))],
            1,
            true,
        ));
        book
    }

    fn make_agent() -> RlQuotePlacementStrategy {
        RlQuotePlacementStrategy::new(dec!(0.01), 5, dec!(10.0), DqnConfig::default())
            .with_seed(42)
    }

    #[test]
    fn test_emits_bid_ask_pair() {
        let mut agent = make_agent();
        let book = make_book(dec!(50000), dec!(50010));
        let sigs = agent.on_book(&book);
        assert_eq!(sigs.len(), 2);
        assert_eq!(sigs[0].side, Side::Buy);
        assert_eq!(sigs[1].side, Side::Sell);
        assert!(sigs[0].price.unwrap() < sigs[1].price.unwrap(), "bid < ask");
    }

    #[test]
    fn test_both_signals_are_post_only_cancel_replace() {
        let mut agent = make_agent();
        let book = make_book(dec!(50000), dec!(50010));
        let sigs = agent.on_book(&book);
        assert_eq!(sigs[0].time_in_force, mercury_core::TimeInForce::PostOnly);
        assert_eq!(sigs[1].time_in_force, mercury_core::TimeInForce::PostOnly);
        assert!(sigs[0].cancel_replace);
        assert!(sigs[1].cancel_replace);
    }

    #[test]
    fn test_fill_updates_inventory() {
        let mut agent = make_agent();
        let fill = Fill {
            order_id: 1,
            exchange: Exchange::Binance,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            price: dec!(50000),
            quantity: dec!(0.1),
            fee: dec!(0),
            fee_asset: "USDT".into(),
            is_maker: true,
            trade_id: 1,
            timestamp: 0,
        };
        agent.on_fill(&fill);
        assert_eq!(agent.inventory, dec!(0.1));
    }

    #[test]
    fn test_replay_buffer_grows_then_caps() {
        let mut agent = make_agent();

        // Each on_book call after the first pushes one experience.
        for i in 0..=REPLAY_CAP + 10 {
            let b = make_book(
                dec!(50000) + Decimal::from(i as i64),
                dec!(50010) + Decimal::from(i as i64),
            );
            agent.on_book(&b);
        }

        assert_eq!(agent.replay_len(), REPLAY_CAP, "replay buffer should be capped at REPLAY_CAP");
    }

    #[test]
    fn test_epsilon_decays_over_ticks() {
        let mut agent = make_agent();
        let initial_eps = agent.epsilon();

        for _ in 0..200 {
            let book = make_book(dec!(50000), dec!(50010));
            agent.on_book(&book);
        }

        assert!(
            agent.epsilon() < initial_eps,
            "epsilon should decay from {initial_eps} but is {}",
            agent.epsilon()
        );
    }

    #[test]
    fn test_training_starts_after_min_replay() {
        let mut agent = make_agent();
        let book = make_book(dec!(50000), dec!(50010));

        // Feed ticks until just below MIN_REPLAY — no training yet.
        for i in 0..MIN_REPLAY {
            let b = make_book(
                dec!(50000) + Decimal::from(i as i64),
                dec!(50010) + Decimal::from(i as i64),
            );
            agent.on_book(&b);
        }
        assert_eq!(agent.train_steps(), 0, "should not train before MIN_REPLAY samples");

        // One more batch of TRAIN_EVERY ticks should trigger training.
        for _ in 0..TRAIN_EVERY as usize {
            agent.on_book(&book);
        }
        assert!(agent.train_steps() > 0, "should have trained after MIN_REPLAY samples");
    }

    #[test]
    fn test_reset_clears_state_keeps_weights() {
        let mut agent = make_agent();
        let book = make_book(dec!(50000), dec!(50010));
        for _ in 0..100 {
            agent.on_book(&book);
        }
        let eps_before = agent.epsilon();

        agent.on_fill(&Fill {
            order_id: 1,
            exchange: Exchange::Binance,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            price: dec!(50000),
            quantity: dec!(1.0),
            fee: dec!(0),
            fee_asset: "USDT".into(),
            is_maker: true,
            trade_id: 1,
            timestamp: 0,
        });

        agent.reset();

        assert_eq!(agent.inventory, Decimal::ZERO, "inventory cleared");
        assert_eq!(agent.session_pnl, Decimal::ZERO, "pnl cleared");
        assert_eq!(agent.prev_mid, None, "prev_mid cleared");
        // ε resets to epsilon_start (fresh episode exploration).
        assert_eq!(agent.epsilon(), DqnConfig::default().epsilon_start);
        // epsilon_start > decayed eps → weights are preserved (ε would otherwise stay decayed)
        assert!(agent.epsilon() > eps_before, "epsilon reset to start");
    }
}
