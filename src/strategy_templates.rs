//! Parameterized strategy templates with structural validation.
//!
//! Each [`StrategyTemplate`] variant knows how to turn a small, strictly
//! validated parameter set into a concrete list of [`Leg`]s against a
//! [`MarketSnapshot`]. The resulting legs can be handed straight to
//! `execute_strategy`, and the accompanying [`StrategyAnalysis`] reports the
//! net premium, bounded/unbounded max profit and max loss, breakevens and
//! margin so clients do not have to reimplement risk math.

use serde::{Deserialize, Serialize};

use crate::payoff::{self, Leg, MarketSnapshot, OptionKind, Side};

/// Error returned when a template cannot be built from the supplied
/// parameters or market snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateError(pub String);

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TemplateError {}

fn err<T>(msg: impl Into<String>) -> Result<T, TemplateError> {
    Err(TemplateError(msg.into()))
}

/// A single option series available in the market snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Series {
    pub expiry: String,
    pub strike: f64,
    pub kind: OptionKind,
    /// Mid premium per contract.
    pub premium: f64,
    /// Absolute delta of the series (used for delta-targeted selection).
    #[serde(default)]
    pub delta: f64,
}

/// Parameters shared by every template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateParams {
    pub underlying: String,
    pub expiry: String,
    /// Center strike. Required unless delta targets are supplied.
    #[serde(default)]
    pub center_strike: Option<f64>,
    /// Target delta for the short put leg (e.g. 0.16).
    #[serde(default)]
    pub put_delta: Option<f64>,
    /// Target delta for the short call leg (e.g. 0.16).
    #[serde(default)]
    pub call_delta: Option<f64>,
    /// Strike width for spreads/condors/butterflies.
    #[serde(default)]
    pub width: Option<f64>,
    /// Number of contracts.
    #[serde(default = "default_contracts")]
    pub contracts: u32,
    /// Second expiry, required for calendars.
    #[serde(default)]
    pub back_expiry: Option<String>,
}

fn default_contracts() -> u32 {
    1
}

/// Named strategy templates. Serialized with an internal `type` tag so the
/// build endpoint can accept `{"type": "iron_condor", ...}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)]
pub enum StrategyTemplate {
    Straddle(TemplateParams),
    Strangle(TemplateParams),
    Vertical(TemplateParams),
    IronCondor(TemplateParams),
    Butterfly(TemplateParams),
    Calendar(TemplateParams),
    Collar(TemplateParams),
    RatioSpread(TemplateParams),
}

impl StrategyTemplate {
    /// Stable identifier persisted as `strategy_type` on execution.
    pub fn strategy_type(&self) -> &'static str {
        match self {
            StrategyTemplate::Straddle(_) => "straddle",
            StrategyTemplate::Strangle(_) => "strangle",
            StrategyTemplate::Vertical(_) => "vertical",
            StrategyTemplate::IronCondor(_) => "iron_condor",
            StrategyTemplate::Butterfly(_) => "butterfly",
            StrategyTemplate::Calendar(_) => "calendar",
            StrategyTemplate::Collar(_) => "collar",
            StrategyTemplate::RatioSpread(_) => "ratio_spread",
        }
    }

    fn params(&self) -> &TemplateParams {
        match self {
            StrategyTemplate::Straddle(p)
            | StrategyTemplate::Strangle(p)
            | StrategyTemplate::Vertical(p)
            | StrategyTemplate::IronCondor(p)
            | StrategyTemplate::Butterfly(p)
            | StrategyTemplate::Calendar(p)
            | StrategyTemplate::Collar(p)
            | StrategyTemplate::RatioSpread(p) => p,
        }
    }

    /// Build the validated legs for this template against `ctx`.
    pub fn legs(&self, ctx: &MarketSnapshot) -> Result<Vec<Leg>, TemplateError> {
        let p = self.params();
        if p.contracts == 0 {
            return err("contracts must be >= 1");
        }
        let qty = p.contracts as i64;
        match self {
            StrategyTemplate::Straddle(_) => {
                let k = self.center(ctx)?;
                let call = ctx.series(&p.expiry, k, OptionKind::Call)?;
                let put = ctx.series(&p.expiry, k, OptionKind::Put)?;
                Ok(vec![
                    Leg::new(put, Side::Sell, qty),
                    Leg::new(call, Side::Sell, qty),
                ])
            }
            StrategyTemplate::Strangle(_) => {
                let put_k = self.delta_strike(ctx, OptionKind::Put, p.put_delta)?;
                let call_k = self.delta_strike(ctx, OptionKind::Call, p.call_delta)?;
                if put_k >= call_k {
                    return err("strangle requires put strike < call strike");
                }
                let put = ctx.series(&p.expiry, put_k, OptionKind::Put)?;
                let call = ctx.series(&p.expiry, call_k, OptionKind::Call)?;
                Ok(vec![
                    Leg::new(put, Side::Sell, qty),
                    Leg::new(call, Side::Sell, qty),
                ])
            }
            StrategyTemplate::Vertical(_) => {
                let width = self.width(p)?;
                let k = self.center(ctx)?;
                let long_k = k - width;
                let short_k = k;
                let long = ctx.series(&p.expiry, long_k, OptionKind::Call)?;
                let short = ctx.series(&p.expiry, short_k, OptionKind::Call)?;
                Ok(vec![
                    Leg::new(long, Side::Buy, qty),
                    Leg::new(short, Side::Sell, qty),
                ])
            }
            StrategyTemplate::IronCondor(_) => {
                let width = self.width(p)?;
                let k = self.center(ctx)?;
                let put_long_k = k - 2.0 * width;
                let put_short_k = k - width;
                let call_short_k = k + width;
                let call_long_k = k + 2.0 * width;
                if !(put_long_k < put_short_k && put_short_k < call_short_k && call_short_k < call_long_k) {
                    return err("iron condor strikes must be strictly increasing");
                }
                let put_long = ctx.series(&p.expiry, put_long_k, OptionKind::Put)?;
                let put_short = ctx.series(&p.expiry, put_short_k, OptionKind::Put)?;
                let call_short = ctx.series(&p.expiry, call_short_k, OptionKind::Call)?;
                let call_long = ctx.series(&p.expiry, call_long_k, OptionKind::Call)?;
                Ok(vec![
                    Leg::new(put_long, Side::Buy, qty),
                    Leg::new(put_short, Side::Sell, qty),
                    Leg::new(call_short, Side::Sell, qty),
                    Leg::new(call_long, Side::Buy, qty),
                ])
            }
            StrategyTemplate::Butterfly(_) => {
                let width = self.width(p)?;
                let k = self.center(ctx)?;
                let lower = ctx.series(&p.expiry, k - width, OptionKind::Call)?;
                let middle = ctx.series(&p.expiry, k, OptionKind::Call)?;
                let upper = ctx.series(&p.expiry, k + width, OptionKind::Call)?;
                Ok(vec![
                    Leg::new(lower, Side::Buy, qty),
                    Leg::new(middle, Side::Sell, 2 * qty),
                    Leg::new(upper, Side::Buy, qty),
                ])
            }
            StrategyTemplate::Calendar(_) => {
                let back = p
                    .back_expiry
                    .as_ref()
                    .ok_or_else(|| TemplateError("calendar requires back_expiry".into()))?;
                if back == &p.expiry {
                    return err("calendar requires distinct front and back expiries");
                }
                let k = self.center(ctx)?;
                let front = ctx.series(&p.expiry, k, OptionKind::Call)?;
                let back_leg = ctx.series(back, k, OptionKind::Call)?;
                Ok(vec![
                    Leg::new(front, Side::Sell, qty),
                    Leg::new(back_leg, Side::Buy, qty),
                ])
            }
            StrategyTemplate::Collar(_) => {
                let width = self.width(p)?;
                let k = self.center(ctx)?;
                let put_k = k - width;
                let call_k = k + width;
                if put_k >= call_k {
                    return err("collar requires put strike < call strike");
                }
                let put = ctx.series(&p.expiry, put_k, OptionKind::Put)?;
                let call = ctx.series(&p.expiry, call_k, OptionKind::Call)?;
                Ok(vec![
                    Leg::new(put, Side::Buy, qty),
                    Leg::new(call, Side::Sell, qty),
                ])
            }
            StrategyTemplate::RatioSpread(_) => {
                let width = self.width(p)?;
                let k = self.center(ctx)?;
                let long = ctx.series(&p.expiry, k, OptionKind::Call)?;
                let short = ctx.series(&p.expiry, k + width, OptionKind::Call)?;
                Ok(vec![
                    Leg::new(long, Side::Buy, qty),
                    Leg::new(short, Side::Sell, 2 * qty),
                ])
            }
        }
    }

    fn center(&self, ctx: &MarketSnapshot) -> Result<f64, TemplateError> {
        let p = self.params();
        if let Some(k) = p.center_strike {
            return Ok(k);
        }
        // Fall back to delta-targeted selection when a center is not given.
        match (p.put_delta, p.call_delta) {
            (Some(pd), Some(cd)) => {
                let put_k = self.delta_strike(ctx, OptionKind::Put, Some(pd))?;
                let call_k = self.delta_strike(ctx, OptionKind::Call, Some(cd))?;
                Ok((put_k + call_k) / 2.0)
            }
            _ => err("center_strike or both delta targets are required"),
        }
    }

    fn width(&self, p: &TemplateParams) -> Result<f64, TemplateError> {
        match p.width {
            Some(w) if w > 0.0 => Ok(w),
            Some(_) => err("width must be positive"),
            None => err("width is required for this template"),
        }
    }

    /// Snap a delta target to the nearest listed series of the given kind.
    fn delta_strike(
        &self,
        ctx: &MarketSnapshot,
        kind: OptionKind,
        target: Option<f64>,
    ) -> Result<f64, TemplateError> {
        let target = target.ok_or_else(|| TemplateError("delta target is required".into()))?;
        if !(0.0..1.0).contains(&target) {
            return err("delta target must be in (0, 1)");
        }
        let p = self.params();
        let mut best: Option<(f64, f64)> = None;
        for s in ctx.series_for(&p.expiry, kind) {
            let dist = (s.delta.abs() - target).abs();
            if best.map_or(true, |(_, d)| dist < d) {
                best = Some((s.strike, dist));
            }
        }
        match best {
            Some((strike, _)) => Ok(strike),
            None => err("no listed series matches the delta target"),
        }
    }
}

/// Risk analysis for a built strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyAnalysis {
    pub net_premium: f64,
    /// `None` when the payoff is unbounded to the upside.
    pub max_profit: Option<f64>,
    /// `None` when the payoff is unbounded to the downside.
    pub max_loss: Option<f64>,
    pub breakevens: Vec<f64>,
    pub margin: f64,
}

/// Build a template into legs and compute its risk analysis.
pub fn build(
    template: &StrategyTemplate,
    ctx: &MarketSnapshot,
) -> Result<(Vec<Leg>, StrategyAnalysis), TemplateError> {
    let legs = template.legs(ctx)?;
    let analysis = analyze(&legs, ctx);
    Ok((legs, analysis))
}

/// Compute net premium, bounded/unbounded max profit and max loss, breakevens
/// and margin for a set of legs.
///
/// Max profit/loss are computed analytically where the payoff is bounded and
/// reported as `None` (unbounded) otherwise. Breakevens are located by
/// root-finding on [`payoff::combined_pnl`].
pub fn analyze(legs: &[Leg], ctx: &MarketSnapshot) -> StrategyAnalysis {
    let net_premium: f64 = legs
        .iter()
        .map(|l| match l.side {
            Side::Sell => l.premium * l.qty as f64,
            Side::Buy => -l.premium * l.qty as f64,
        })
        .sum();

    let strikes: Vec<f64> = legs.iter().map(|l| l.strike).collect();
    let lo = strikes.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = strikes.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let span = (hi - lo).max(1.0);

    // Sample the payoff across a grid that extends well past the strikes so we
    // can detect unbounded tails and locate breakevens.
    let grid_lo = lo - span;
    let grid_hi = hi + span;
    let steps = 400usize;
    let step = (grid_hi - grid_lo) / steps as f64;

    let mut breakevens = Vec::new();
    let mut prev_x = grid_lo;
    let mut prev_y = payoff::combined_pnl(legs, prev_x);
    for i in 1..=steps {
        let x = grid_lo + step * i as f64;
        let y = payoff::combined_pnl(legs, x);
        if prev_y == 0.0 {
            breakevens.push(prev_x);
        } else if prev_y.signum() != y.signum() {
            // Bisection refine between prev_x and x.
            let (mut a, mut b) = (prev_x, x);
            let (mut fa, _) = (prev_y, y);
            for _ in 0..60 {
                let m = 0.5 * (a + b);
                let fm = payoff::combined_pnl(legs, m);
                if fa.signum() == fm.signum() {
                    a = m;
                    fa = fm;
                } else {
                    b = m;
                }
            }
            breakevens.push(0.5 * (a + b));
        }
        prev_x = x;
        prev_y = y;
    }

    // Boundedness: compare the slope of the payoff in the far tails.
    let left_slope = payoff::combined_pnl(legs, grid_lo) - payoff::combined_pnl(legs, grid_lo - span);
    let right_slope = payoff::combined_pnl(legs, grid_hi + span) - payoff::combined_pnl(legs, grid_hi);

    let mut max_profit = f64::NEG_INFINITY;
    let mut max_loss = f64::INFINITY;
    for i in 0..=steps {
        let x = grid_lo + step * i as f64;
        let y = payoff::combined_pnl(legs, x);
        max_profit = max_profit.max(y);
        max_loss = max_loss.min(y);
    }

    let max_profit = if right_slope > 0.0 { None } else { Some(max_profit) };
    let max_loss = if left_slope < 0.0 { None } else { Some(max_loss) };

    let margin = margin_requirement(legs, ctx);

    StrategyAnalysis {
        net_premium,
        max_profit,
        max_loss,
        breakevens,
        margin,
    }
}

/// Defined-risk margin: the worst-case loss of the structure, floored at the
/// net debit paid. Unbounded structures fall back to a notional-based estimate.
fn margin_requirement(legs: &[Leg], ctx: &MarketSnapshot) -> f64 {
    let analysis = analyze_bounded(legs);
    match analysis {
        Some(worst) => worst.max(0.0),
        None => {
            let notional: f64 = legs
                .iter()
                .map(|l| l.strike * l.qty as f64 * ctx.contract_multiplier)
                .sum();
            notional.abs() * 0.2
        }
    }
}

/// Worst-case loss for bounded structures, or `None` when unbounded.
fn analyze_bounded(legs: &[Leg]) -> Option<f64> {
    let strikes: Vec<f64> = legs.iter().map(|l| l.strike).collect();
    let lo = strikes.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = strikes.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let span = (hi - lo).max(1.0);
    let left_slope = payoff::combined_pnl(legs, lo - span) - payoff::combined_pnl(legs, lo - 2.0 * span);
    let right_slope = payoff::combined_pnl(legs, hi + 2.0 * span) - payoff::combined_pnl(legs, hi + span);
    if left_slope < 0.0 || right_slope > 0.0 {
        return None;
    }
    let mut worst = f64::INFINITY;
    for i in 0..=400 {
        let x = lo - span + (2.0 * span + (hi - lo)) * i as f64 / 400.0;
        worst = worst.min(payoff::combined_pnl(legs, x));
    }
    Some(-worst)
}
