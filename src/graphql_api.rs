use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use async_graphql::dataloader::{DataLoader, Loader};
use async_graphql::{
    Context, EmptyMutation, EmptySubscription, Error, Object, Schema, SimpleObject,
};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Json;
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::models::{Account, Position};
use crate::{black_scholes, smile_vol, AppState, BSInputs};

const MAX_POSITIONS_LIMIT: i32 = 200;
const DEFAULT_POSITIONS_LIMIT: i32 = 50;
const MAX_EXPIRY_DAYS: f64 = 3650.0;
const MAX_QUERY_DEPTH: usize = 12;
const MAX_QUERY_COMPLEXITY: usize = 1000;

pub type GraphQLSchema = Schema<QueryRoot, EmptyMutation, EmptySubscription>;

static SCHEMA: OnceLock<GraphQLSchema> = OnceLock::new();

pub fn build_schema() -> GraphQLSchema {
    Schema::build(QueryRoot, EmptyMutation, EmptySubscription)
        .limit_depth(MAX_QUERY_DEPTH)
        .limit_complexity(MAX_QUERY_COMPLEXITY)
        .finish()
}

fn schema() -> &'static GraphQLSchema {
    SCHEMA.get_or_init(build_schema)
}

#[derive(Clone)]
struct Viewer(String);

struct AccountLoader {
    db: SqlitePool,
}

impl Loader<String> for AccountLoader {
    type Value = Account;
    type Error = String;

    async fn load(&self, keys: &[String]) -> Result<HashMap<String, Self::Value>, Self::Error> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }

        let mut query =
            QueryBuilder::<Sqlite>::new("SELECT * FROM accounts WHERE wallet_address IN (");
        {
            let mut separated = query.separated(", ");
            for key in keys {
                separated.push_bind(key);
            }
        }
        query.push(")");

        let rows: Vec<Account> = query
            .build_query_as()
            .fetch_all(&self.db)
            .await
            .map_err(|error| error.to_string())?;
        Ok(rows
            .into_iter()
            .map(|account| (account.wallet_address.clone(), account))
            .collect())
    }
}

struct PositionsLoader {
    db: SqlitePool,
}

impl Loader<String> for PositionsLoader {
    type Value = Vec<Position>;
    type Error = String;

    async fn load(&self, keys: &[String]) -> Result<HashMap<String, Self::Value>, Self::Error> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }

        let mut query =
            QueryBuilder::<Sqlite>::new("SELECT * FROM positions WHERE wallet_address IN (");
        {
            let mut separated = query.separated(", ");
            for key in keys {
                separated.push_bind(key);
            }
        }
        query.push(") ORDER BY opened_at ASC");

        let rows: Vec<Position> = query
            .build_query_as()
            .fetch_all(&self.db)
            .await
            .map_err(|error| error.to_string())?;
        let requested: HashSet<&str> = keys.iter().map(String::as_str).collect();
        let mut grouped = HashMap::new();
        for wallet in requested {
            grouped.insert(wallet.to_owned(), Vec::new());
        }
        for position in rows {
            grouped
                .entry(position.wallet_address.clone())
                .or_insert_with(Vec::new)
                .push(position);
        }
        Ok(grouped)
    }
}

fn account_loader<'a>(ctx: &'a Context<'_>) -> Result<&'a DataLoader<AccountLoader>, Error> {
    ctx.data::<DataLoader<AccountLoader>>()
}

fn positions_loader<'a>(ctx: &'a Context<'_>) -> Result<&'a DataLoader<PositionsLoader>, Error> {
    ctx.data::<DataLoader<PositionsLoader>>()
}

async fn current_positions(
    ctx: &Context<'_>,
    wallet_address: &str,
) -> Result<Vec<Position>, Error> {
    positions_loader(ctx)?
        .load_one(wallet_address.to_owned())
        .await
        .map(|positions| positions.unwrap_or_default())
        .map_err(|error| {
            tracing::error!(%error, "GraphQL positions loader failed");
            Error::new("could not load portfolio data")
        })
}

fn authenticate_bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

async fn session_wallet(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let token = authenticate_bearer_token(headers)?;
    match sqlx::query_scalar::<_, String>(
        "SELECT wallet_address FROM sessions
         WHERE token = ? AND expires_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
    )
    .bind(token)
    .fetch_optional(&state.db)
    .await
    {
        Ok(wallet) => wallet,
        Err(error) => {
            tracing::warn!(%error, "GraphQL session lookup failed");
            None
        }
    }
}

/// Axum handler for `POST /graphql`. The bearer token is resolved through
/// the sessions table; no GraphQL argument can select a portfolio owner.
pub async fn graphql_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<async_graphql::Request>,
) -> Json<async_graphql::Response> {
    let viewer = session_wallet(&state, &headers).await;
    let request = request
        .data(state.clone())
        .data(DataLoader::new(
            AccountLoader {
                db: state.db.clone(),
            },
            tokio::spawn,
        ))
        .data(DataLoader::new(
            PositionsLoader {
                db: state.db.clone(),
            },
            tokio::spawn,
        ));
    let request = match viewer {
        Some(wallet) => request.data(Viewer(wallet)),
        None => request,
    };

    Json(schema().execute(request).await)
}

#[derive(SimpleObject, Clone)]
pub struct SpotQuote {
    pub underlying: String,
    pub price: f64,
    pub volatility: Option<f64>,
}

#[derive(Clone)]
pub struct Market {
    underlying: String,
    spot: f64,
    volatility: f64,
}

#[derive(SimpleObject, Clone)]
pub struct OptionQuote {
    pub premium: f64,
    pub delta: f64,
    pub gamma: f64,
    pub theta: f64,
    pub vega: f64,
    pub rho: f64,
    pub intrinsic: f64,
    pub time_value: f64,
    pub implied_volatility: f64,
}

impl From<crate::BSResult> for OptionQuote {
    fn from(result: crate::BSResult) -> Self {
        Self {
            premium: result.premium,
            delta: result.delta,
            gamma: result.gamma,
            theta: result.theta,
            vega: result.vega,
            rho: result.rho,
            intrinsic: result.intrinsic,
            time_value: result.time_value,
            implied_volatility: result.iv,
        }
    }
}

#[derive(SimpleObject, Clone)]
pub struct OptionChainEntry {
    pub strike: f64,
    pub expiry_days: f64,
    pub call: OptionQuote,
    pub put: OptionQuote,
    pub is_itm_call: bool,
    pub is_itm_put: bool,
}

#[derive(SimpleObject, Clone)]
pub struct VolatilityPoint {
    pub expiry_days: f64,
    pub strike: f64,
    pub volatility: f64,
}

#[derive(SimpleObject, Clone)]
pub struct VolatilitySurface {
    pub underlying: String,
    pub base_volatility: f64,
    pub points: Vec<VolatilityPoint>,
}

fn checked_expiry(expiry_days: Option<f64>) -> Result<f64, Error> {
    let expiry_days = expiry_days.unwrap_or(30.0);
    if !expiry_days.is_finite() || expiry_days <= 0.0 || expiry_days > MAX_EXPIRY_DAYS {
        return Err(Error::new(format!(
            "expiry_days must be greater than zero and no more than {MAX_EXPIRY_DAYS}"
        )));
    }
    Ok(expiry_days)
}

fn option_chain(spot: f64, base_volatility: f64, expiry_days: f64) -> Vec<OptionChainEntry> {
    let t = expiry_days / 365.0;
    (-7..=7)
        .filter_map(|step| {
            let strike = (spot * (1.0 + step as f64 * 0.05) * 10_000.0).round() / 10_000.0;
            if strike <= 0.0 {
                return None;
            }
            let volatility = smile_vol(base_volatility, strike / spot);
            let call = black_scholes(&BSInputs {
                spot,
                strike,
                vol: volatility,
                t,
                r: 0.05,
                is_call: true,
            });
            let put = black_scholes(&BSInputs {
                spot,
                strike,
                vol: volatility,
                t,
                r: 0.05,
                is_call: false,
            });
            Some(OptionChainEntry {
                strike,
                expiry_days,
                call: call.into(),
                put: put.into(),
                is_itm_call: spot > strike,
                is_itm_put: spot < strike,
            })
        })
        .collect()
}

fn volatility_surface(underlying: &str, spot: f64, base_volatility: f64) -> VolatilitySurface {
    let expiries = [7.0, 30.0, 90.0, 180.0, 365.0];
    let points = expiries
        .into_iter()
        .flat_map(|expiry_days| {
            (-7..=7).filter_map(move |step| {
                let strike = (spot * (1.0 + step as f64 * 0.05) * 10_000.0).round() / 10_000.0;
                (strike > 0.0).then(|| VolatilityPoint {
                    expiry_days,
                    strike,
                    volatility: smile_vol(base_volatility, strike / spot),
                })
            })
        })
        .collect();
    VolatilitySurface {
        underlying: underlying.to_owned(),
        base_volatility,
        points,
    }
}

fn market_from_state(state: &AppState, underlying: &str) -> Option<Market> {
    let spot = *state.spot_prices.lock().unwrap().get(underlying)?;
    let volatility = *state.vol_surface.lock().unwrap().get(underlying)?;
    Some(Market {
        underlying: underlying.to_owned(),
        spot,
        volatility,
    })
}

#[Object]
impl Market {
    async fn underlying(&self) -> &str {
        &self.underlying
    }

    async fn spot(&self) -> f64 {
        self.spot
    }

    async fn volatility(&self) -> f64 {
        self.volatility
    }

    async fn option_chain(&self, expiry_days: Option<f64>) -> Result<Vec<OptionChainEntry>, Error> {
        Ok(option_chain(
            self.spot,
            self.volatility,
            checked_expiry(expiry_days)?,
        ))
    }

    async fn volatility_surface(&self) -> VolatilitySurface {
        volatility_surface(&self.underlying, self.spot, self.volatility)
    }
}

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn spot_prices(&self, ctx: &Context<'_>) -> Result<Vec<SpotQuote>, Error> {
        let state = ctx.data::<AppState>()?;
        let prices = state.spot_prices.lock().unwrap().clone();
        let volatilities = state.vol_surface.lock().unwrap().clone();
        let mut underlying_names: Vec<_> = prices.keys().cloned().collect();
        underlying_names.sort();
        Ok(underlying_names
            .into_iter()
            .filter_map(|underlying| {
                prices.get(&underlying).map(|price| SpotQuote {
                    underlying: underlying.clone(),
                    price: *price,
                    volatility: volatilities.get(&underlying).copied(),
                })
            })
            .collect())
    }

    async fn markets(&self, ctx: &Context<'_>) -> Result<Vec<Market>, Error> {
        let state = ctx.data::<AppState>()?;
        let mut underlying_names: Vec<_> =
            state.spot_prices.lock().unwrap().keys().cloned().collect();
        underlying_names.sort();
        Ok(underlying_names
            .iter()
            .filter_map(|underlying| market_from_state(state, underlying))
            .collect())
    }

    async fn market(&self, ctx: &Context<'_>, underlying: String) -> Result<Option<Market>, Error> {
        Ok(market_from_state(ctx.data::<AppState>()?, &underlying))
    }

    async fn option_chain(
        &self,
        ctx: &Context<'_>,
        underlying: String,
        expiry_days: Option<f64>,
    ) -> Result<Vec<OptionChainEntry>, Error> {
        let market = market_from_state(ctx.data::<AppState>()?, &underlying)
            .ok_or_else(|| Error::new("unknown underlying"))?;
        Ok(option_chain(
            market.spot,
            market.volatility,
            checked_expiry(expiry_days)?,
        ))
    }

    async fn volatility_surface(
        &self,
        ctx: &Context<'_>,
        underlying: String,
    ) -> Result<Option<VolatilitySurface>, Error> {
        Ok(market_from_state(ctx.data::<AppState>()?, &underlying)
            .map(|market| volatility_surface(&market.underlying, market.spot, market.volatility)))
    }

    async fn portfolio(&self, ctx: &Context<'_>) -> Result<Portfolio, Error> {
        let viewer = ctx
            .data_opt::<Viewer>()
            .ok_or_else(|| Error::new("authentication required"))?;
        Ok(Portfolio {
            wallet_address: viewer.0.clone(),
        })
    }
}

#[derive(Clone)]
pub struct Portfolio {
    wallet_address: String,
}

#[derive(SimpleObject, Clone)]
pub struct PortfolioAccount {
    pub wallet_address: String,
    pub balance: f64,
    pub collateral_locked: f64,
    pub created_at: String,
}

impl From<Account> for PortfolioAccount {
    fn from(account: Account) -> Self {
        Self {
            wallet_address: account.wallet_address,
            balance: account.balance,
            collateral_locked: account.collateral_locked,
            created_at: account.created_at,
        }
    }
}

#[derive(SimpleObject, Clone)]
#[graphql(complex)]
pub struct PositionNode {
    pub id: String,
    pub underlying: String,
    pub strike: f64,
    pub expiry_days: f64,
    pub option_type: String,
    pub position_type: String,
    pub contracts: f64,
    pub entry_premium: f64,
    pub entry_spot: f64,
    pub collateral: f64,
    pub status: String,
    pub close_premium: Option<f64>,
    pub close_spot: Option<f64>,
    pub realized_pnl: Option<f64>,
    pub opened_at: String,
    pub closed_at: Option<String>,
    pub strategy_id: Option<String>,
    #[graphql(skip)]
    wallet_address: String,
}

impl From<Position> for PositionNode {
    fn from(position: Position) -> Self {
        Self {
            id: position.id,
            underlying: position.underlying,
            strike: position.strike,
            expiry_days: position.expiry_days,
            option_type: position.option_type,
            position_type: position.position_type,
            contracts: position.contracts,
            entry_premium: position.entry_premium,
            entry_spot: position.entry_spot,
            collateral: position.collateral,
            status: position.status,
            close_premium: position.close_premium,
            close_spot: position.close_spot,
            realized_pnl: position.realized_pnl,
            opened_at: position.opened_at,
            closed_at: position.closed_at,
            strategy_id: position.strategy_id,
            wallet_address: position.wallet_address,
        }
    }
}

#[derive(Clone, SimpleObject)]
#[graphql(complex)]
pub struct Strategy {
    pub strategy_id: String,
    pub underlying: String,
    pub leg_count: i32,
    pub open_leg_count: i32,
    pub status: String,
    pub opened_at: String,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    #[graphql(skip)]
    wallet_address: String,
}

impl Strategy {
    fn from_legs(
        state: &AppState,
        wallet_address: String,
        strategy_id: String,
        legs: &[Position],
    ) -> Self {
        let open_leg_count = legs
            .iter()
            .filter(|position| position.status == "open")
            .count();
        let realized_pnl = legs
            .iter()
            .filter_map(|position| position.realized_pnl)
            .sum();
        let unrealized_pnl = legs
            .iter()
            .filter(|position| position.status == "open")
            .map(|position| {
                let Some(result) = crate::positions::current_bs_result(state, position) else {
                    return 0.0;
                };
                if position.position_type == "short" {
                    (position.entry_premium - result.premium) * position.contracts
                } else {
                    (result.premium - position.entry_premium) * position.contracts
                }
            })
            .sum();
        Self {
            strategy_id,
            underlying: legs
                .first()
                .map(|position| position.underlying.clone())
                .unwrap_or_default(),
            leg_count: legs.len().min(i32::MAX as usize) as i32,
            open_leg_count: open_leg_count.min(i32::MAX as usize) as i32,
            status: if open_leg_count > 0 { "open" } else { "closed" }.to_owned(),
            opened_at: legs
                .first()
                .map(|position| position.opened_at.clone())
                .unwrap_or_default(),
            realized_pnl,
            unrealized_pnl,
            wallet_address,
        }
    }
}

#[async_graphql::ComplexObject]
impl PositionNode {
    async fn market(&self, ctx: &Context<'_>) -> Result<Option<Market>, Error> {
        Ok(market_from_state(ctx.data::<AppState>()?, &self.underlying))
    }

    async fn strategy(&self, ctx: &Context<'_>) -> Result<Option<Strategy>, Error> {
        let Some(strategy_id) = &self.strategy_id else {
            return Ok(None);
        };
        let positions = current_positions(ctx, &self.wallet_address).await?;
        let legs: Vec<_> = positions
            .iter()
            .filter(|position| position.strategy_id.as_deref() == Some(strategy_id.as_str()))
            .cloned()
            .collect();
        if legs.is_empty() {
            return Ok(None);
        }
        Ok(Some(Strategy::from_legs(
            ctx.data::<AppState>()?,
            self.wallet_address.clone(),
            strategy_id.clone(),
            &legs,
        )))
    }
}

#[async_graphql::ComplexObject]
impl Strategy {
    async fn legs(&self, ctx: &Context<'_>) -> Result<Vec<PositionNode>, Error> {
        let mut legs: Vec<_> = current_positions(ctx, &self.wallet_address)
            .await?
            .into_iter()
            .filter(|position| position.strategy_id.as_deref() == Some(self.strategy_id.as_str()))
            .map(PositionNode::from)
            .collect();
        legs.sort_by(|left, right| left.opened_at.cmp(&right.opened_at));
        Ok(legs)
    }
}

#[derive(SimpleObject, Clone)]
pub struct PositionsConnection {
    pub positions: Vec<PositionNode>,
    pub total_count: i64,
    pub has_more: bool,
}

#[derive(SimpleObject, Clone)]
pub struct HistoryStats {
    pub trade_count: i64,
    pub win_count: i64,
    pub loss_count: i64,
    pub total_realized_pnl: f64,
}

#[derive(SimpleObject, Clone)]
pub struct History {
    pub trades: Vec<PositionNode>,
    pub stats: HistoryStats,
    pub has_more: bool,
}

fn pagination(limit: Option<i32>, offset: Option<i32>) -> (usize, usize) {
    (
        limit
            .unwrap_or(DEFAULT_POSITIONS_LIMIT)
            .clamp(1, MAX_POSITIONS_LIMIT) as usize,
        offset.unwrap_or(0).max(0) as usize,
    )
}

fn newest_first(mut positions: Vec<Position>) -> Vec<Position> {
    positions.sort_by(|left, right| right.opened_at.cmp(&left.opened_at));
    positions
}

#[Object]
impl Portfolio {
    async fn account(&self, ctx: &Context<'_>) -> Result<Option<PortfolioAccount>, Error> {
        account_loader(ctx)?
            .load_one(self.wallet_address.clone())
            .await
            .map(|account| account.map(PortfolioAccount::from))
            .map_err(|error| {
                tracing::error!(%error, "GraphQL account loader failed");
                Error::new("could not load portfolio account")
            })
    }

    async fn positions(
        &self,
        ctx: &Context<'_>,
        status: Option<String>,
        strategy_id: Option<String>,
        limit: Option<i32>,
        offset: Option<i32>,
    ) -> Result<PositionsConnection, Error> {
        if let Some(status) = &status {
            if !["open", "closed", "rolled"].contains(&status.as_str()) {
                return Err(Error::new("status must be open, closed, or rolled"));
            }
        }

        let all = newest_first(current_positions(ctx, &self.wallet_address).await?);
        let matching: Vec<_> = all
            .into_iter()
            .filter(|position| {
                status
                    .as_ref()
                    .is_none_or(|status| position.status.as_str() == status.as_str())
                    && strategy_id
                        .as_ref()
                        .is_none_or(|id| position.strategy_id.as_deref() == Some(id.as_str()))
            })
            .collect();
        let total_count = matching.len() as i64;
        let (limit, offset) = pagination(limit, offset);
        let positions = matching
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(PositionNode::from)
            .collect::<Vec<_>>();
        let has_more = ((offset + positions.len()) as i64) < total_count;
        Ok(PositionsConnection {
            positions,
            total_count,
            has_more,
        })
    }

    async fn strategies(&self, ctx: &Context<'_>) -> Result<Vec<Strategy>, Error> {
        let state = ctx.data::<AppState>()?;
        let positions = current_positions(ctx, &self.wallet_address).await?;
        let mut groups: HashMap<String, Vec<Position>> = HashMap::new();
        for position in positions {
            if let Some(strategy_id) = &position.strategy_id {
                groups
                    .entry(strategy_id.clone())
                    .or_default()
                    .push(position);
            }
        }

        let mut strategies: Vec<_> = groups
            .into_iter()
            .map(|(strategy_id, mut legs)| {
                legs.sort_by(|left, right| left.opened_at.cmp(&right.opened_at));
                Strategy::from_legs(state, self.wallet_address.clone(), strategy_id, &legs)
            })
            .collect();
        strategies.sort_by(|left, right| right.opened_at.cmp(&left.opened_at));
        Ok(strategies)
    }

    async fn history(
        &self,
        ctx: &Context<'_>,
        limit: Option<i32>,
        offset: Option<i32>,
    ) -> Result<History, Error> {
        let (limit, offset) = pagination(limit, offset);
        let mut trades: Vec<_> = current_positions(ctx, &self.wallet_address)
            .await?
            .into_iter()
            .filter(|position| position.status == "closed" || position.status == "rolled")
            .collect();
        trades.sort_by(|left, right| {
            right
                .closed_at
                .as_deref()
                .unwrap_or_default()
                .cmp(left.closed_at.as_deref().unwrap_or_default())
        });
        let trade_count = trades.len() as i64;
        let win_count = trades
            .iter()
            .filter(|position| position.realized_pnl.unwrap_or(0.0) > 0.0)
            .count() as i64;
        let loss_count = trades
            .iter()
            .filter(|position| position.realized_pnl.unwrap_or(0.0) < 0.0)
            .count() as i64;
        let total_realized_pnl = trades
            .iter()
            .filter_map(|position| position.realized_pnl)
            .sum();
        let trades: Vec<_> = trades
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(PositionNode::from)
            .collect();
        let has_more = ((offset + trades.len()) as i64) < trade_count;
        Ok(History {
            trades,
            stats: HistoryStats {
                trade_count,
                win_count,
                loss_count,
                total_realized_pnl,
            },
            has_more,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_market_calculations_return_bounded_chain_and_surface() {
        let chain = option_chain(100.0, 0.5, 30.0);
        assert_eq!(chain.len(), 15);
        assert!(chain.iter().all(|entry| entry.call.premium >= 0.0));
        assert!(chain.iter().all(|entry| entry.put.premium >= 0.0));

        let surface = volatility_surface("TEST", 100.0, 0.5);
        assert_eq!(surface.points.len(), 75);
        assert!(surface.points.iter().all(|point| point.volatility > 0.0));
    }

    #[tokio::test]
    async fn schema_has_no_mutation_root() {
        let response = build_schema().execute("mutation { write }").await;
        assert!(!response.errors.is_empty());
    }

    #[tokio::test]
    async fn schema_rejects_queries_over_the_complexity_budget() {
        let fields = (0..=MAX_QUERY_COMPLEXITY)
            .map(|index| format!("field{index}: spotPrices {{ underlying }}"))
            .collect::<Vec<_>>()
            .join(" ");
        let response = build_schema()
            .execute(format!("query {{ {fields} }}"))
            .await;
        assert!(!response.errors.is_empty());
    }
}
