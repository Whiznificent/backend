use axum::response::Json;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

struct SecurityAddon;

#[utoipa::path(get, path = "/api/v1/openapi.json", responses((status = 200, description = "OpenAPI 3.1 specification", content_type = "application/json")))]
pub async fn openapi_document() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        openapi
            .components
            .get_or_insert_with(Default::default)
            .add_security_scheme(
                "bearerAuth",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("opaque session token")
                        .build(),
                ),
            );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Zenith Backend API",
        version = "0.1.0",
        description = "Paper-trading API for the Zenith options protocol."
    ),
    paths(
        crate::health,
        crate::get_spot,
        crate::price_option,
        crate::get_chain,
        crate::get_implied_vol,
        crate::get_expiry_calendar,
        crate::get_protocol_stats,
        crate::auth::post_nonce,
        crate::auth::post_verify,
        crate::auth::get_me,
        crate::positions::get_account,
        crate::positions::list_positions,
        crate::positions::open_position,
        crate::positions::close_position,
        crate::positions::roll_position,
        crate::positions::get_portfolio_greeks,
        crate::history::get_history,
        crate::watchlist::get_watchlist,
        crate::watchlist::add_watchlist,
        crate::watchlist::remove_watchlist,
        crate::alerts::get_alerts,
        crate::alerts::create_alert,
        crate::alerts::delete_alert,
        crate::strategies::execute_strategy,
        crate::strategies::list_strategies,
        crate::strategies::get_strategy,
        crate::strategies::close_strategy,
        crate::payoff::post_payoff,
        crate::prices::ws_spot,
        crate::openapi::openapi_document
    ),
    components(schemas(
        crate::BSResult,
        crate::PriceQuery,
        crate::IvQuery,
        crate::IvResult,
        crate::OptionChainEntry,
        crate::ChainQuery,
        crate::ExpiryCalendar,
        crate::ExpiryInfo,
        crate::SpotResponse,
        crate::HealthResponse,
        crate::ProtocolStatsResponse,
        crate::error::ErrorResponse,
        crate::models::Account,
        crate::models::Position,
        crate::models::WatchlistItem,
        crate::models::Alert,
        crate::auth::NonceRequest,
        crate::auth::NonceResponse,
        crate::auth::VerifyRequest,
        crate::auth::VerifyResponse,
        crate::auth::MeResponse,
        crate::positions::ListPositionsQuery,
        crate::positions::OpenPositionRequest,
        crate::positions::RollPositionRequest,
        crate::positions::RollResult,
        crate::positions::AggregateGreeks,
        crate::history::HistoryQuery,
        crate::history::HistoryStats,
        crate::history::HistoryResponse,
        crate::watchlist::AddWatchlistRequest,
        crate::alerts::CreateAlertRequest,
        crate::strategies::ExecuteStrategyRequest,
        crate::strategies::StrategySummary,
        crate::strategies::StrategyDetail,
        crate::payoff::PayoffRequest,
        crate::payoff::PayoffResponse,
        crate::payoff::PricedLeg,
        crate::payoff::PayoffPoint
    )),
    modifiers(&SecurityAddon)
)]
pub struct ApiDoc;
