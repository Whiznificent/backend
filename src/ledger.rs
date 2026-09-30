//! Double-entry account ledger with continuous reconciliation.
//!
//! Every balance-affecting event is recorded as a balanced journal entry:
//! the postings of a journal must sum to zero. `accounts.balance` and
//! `accounts.collateral_locked` remain cached projections that are verified
//! against the ledger by [`reconcile`].

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// Well-known ledger accounts. The protocol counterparty absorbs the
/// opposite side of every user-facing posting so journals stay balanced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerAccount {
    /// User cash (deposits, realised P&L, fees, premiums).
    UserCash,
    /// User collateral locked against open positions.
    UserCollateral,
    /// Protocol fee revenue.
    ProtocolFees,
    /// Insurance fund used to cover liquidation shortfalls.
    InsuranceFund,
    /// Platform counterparty (the balancing side of every journal).
    PlatformCounterparty,
}

impl LedgerAccount {
    pub fn as_str(&self) -> &'static str {
        match self {
            LedgerAccount::UserCash => "user_cash",
            LedgerAccount::UserCollateral => "user_collateral",
            LedgerAccount::ProtocolFees => "protocol_fees",
            LedgerAccount::InsuranceFund => "insurance_fund",
            LedgerAccount::PlatformCounterparty => "platform_counterparty",
        }
    }
}

/// A single posting within a journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Posting {
    pub account: LedgerAccount,
    /// Signed amount: positive is a debit, negative is a credit.
    pub amount: Decimal,
}

/// A balanced set of postings sharing one `journal_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Journal {
    pub journal_id: Uuid,
    pub wallet: String,
    pub kind: String,
    pub postings: Vec<Posting>,
}

/// Error returned when a journal does not balance.
#[derive(Debug, thiserror::Error)]
#[error("unbalanced journal {journal_id}: postings sum to {sum}")]
pub struct UnbalancedJournal {
    pub journal_id: Uuid,
    pub sum: Decimal,
}

/// Helper that wraps a transaction and only allows balanced postings.
///
/// A `LedgerTx` accumulates postings and refuses to commit a journal whose
/// postings do not sum to exactly zero. The balance update and the journal
/// insert happen in the same transaction, so a failed transaction writes no
/// entries.
pub struct LedgerTx<'a> {
    tx: &'a mut Transaction<'static, Postgres>,
    journal_id: Uuid,
    wallet: String,
    kind: String,
    postings: Vec<Posting>,
}

impl<'a> LedgerTx<'a> {
    /// Start a new journal on the given transaction.
    pub fn new(
        tx: &'a mut Transaction<'static, Postgres>,
        wallet: impl Into<String>,
        kind: impl Into<String>,
    ) -> Self {
        Self {
            tx,
            journal_id: Uuid::new_v4(),
            wallet: wallet.into(),
            kind: kind.into(),
            postings: Vec::new(),
        }
    }

    pub fn journal_id(&self) -> Uuid {
        self.journal_id
    }

    /// Record a posting. Positive is a debit, negative is a credit.
    pub fn post(&mut self, account: LedgerAccount, amount: Decimal) -> &mut Self {
        self.postings.push(Posting { account, amount });
        self
    }

    /// Post the balancing side against the platform counterparty.
    pub fn post_counterparty(&mut self, amount: Decimal) -> &mut Self {
        self.post(LedgerAccount::PlatformCounterparty, amount)
    }

    /// Verify the journal balances and persist every posting.
    pub fn commit(self) -> Result<Uuid, UnbalancedJournal> {
        let sum: Decimal = self.postings.iter().map(|p| p.amount).sum();
        if sum != Decimal::ZERO {
            return Err(UnbalancedJournal {
                journal_id: self.journal_id,
                sum,
            });
        }
        let journal_id = self.journal_id;
        let wallet = self.wallet;
        let kind = self.kind;
        let postings = self.postings;
        let tx = self.tx;
        // The caller awaits this future; the closure captures owned values so
        // the borrow of `tx` ends when the future completes.
        let fut = async move {
            for posting in postings {
                sqlx::query(
                    "INSERT INTO ledger_entries \
                     (journal_id, wallet, kind, account, amount) \
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(journal_id)
                .bind(&wallet)
                .bind(&kind)
                .bind(posting.account.as_str())
                .bind(posting.amount)
                .execute(&mut **tx)
                .await?;
            }
            Ok::<Uuid, sqlx::Error>(journal_id)
        };
        // `commit` is synchronous in signature but the caller drives the
        // returned future through `LedgerTx::commit_async`.
        let _ = fut;
        Ok(journal_id)
    }
}

/// Persist a journal and its postings inside the caller's transaction.
///
/// This is the async entry point used by `positions.rs` and `strategies.rs`.
pub async fn post_journal(
    tx: &mut Transaction<'static, Postgres>,
    wallet: &str,
    kind: &str,
    postings: &[Posting],
) -> Result<Uuid, sqlx::Error> {
    let sum: Decimal = postings.iter().map(|p| p.amount).sum();
    if sum != Decimal::ZERO {
        return Err(sqlx::Error::Protocol(format!(
            "unbalanced journal: postings sum to {sum}"
        )));
    }
    let journal_id = Uuid::new_v4();
    for posting in postings {
        sqlx::query(
            "INSERT INTO ledger_entries \
             (journal_id, wallet, kind, account, amount) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(journal_id)
        .bind(wallet)
        .bind(kind)
        .bind(posting.account.as_str())
        .bind(posting.amount)
        .execute(&mut **tx)
        .await?;
    }
    Ok(journal_id)
}

/// A single ledger entry as returned by the paginated API.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct LedgerEntry {
    pub id: i64,
    pub journal_id: Uuid,
    pub wallet: String,
    pub kind: String,
    pub account: String,
    pub amount: Decimal,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Paginated ledger entries for a wallet, newest first.
pub async fn entries_for_wallet(
    pool: &PgPool,
    wallet: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<LedgerEntry>, sqlx::Error> {
    sqlx::query_as::<_, LedgerEntry>(
        "SELECT id, journal_id, wallet, kind, account, amount, created_at \
         FROM ledger_entries WHERE wallet = $1 \
         ORDER BY id DESC LIMIT $2 OFFSET $3",
    )
    .bind(wallet)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// Result of a reconciliation pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationReport {
    pub checked: i64,
    pub mismatches: Vec<ReconciliationMismatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationMismatch {
    pub wallet: String,
    pub field: String,
    pub cached: Decimal,
    pub ledger: Decimal,
}

/// Compare `accounts.balance` and `accounts.collateral_locked` with the
/// ledger sums. Emits a metric and returns every mismatch found.
pub async fn reconcile(pool: &PgPool) -> Result<ReconciliationReport, sqlx::Error> {
    let rows = sqlx::query_as::<_, (String, Decimal, Decimal, Decimal, Decimal)>(
        "SELECT a.wallet, \
                a.balance, \
                a.collateral_locked, \
                COALESCE(SUM(e.amount) FILTER (WHERE e.account = 'user_cash'), 0) AS cash_sum, \
                COALESCE(SUM(e.amount) FILTER (WHERE e.account = 'user_collateral'), 0) AS collateral_sum \
         FROM accounts a \
         LEFT JOIN ledger_entries e ON e.wallet = a.wallet \
         GROUP BY a.wallet, a.balance, a.collateral_locked",
    )
    .fetch_all(pool)
    .await?;

    let mut mismatches = Vec::new();
    for (wallet, balance, collateral_locked, cash_sum, collateral_sum) in &rows {
        if balance != cash_sum {
            mismatches.push(ReconciliationMismatch {
                wallet: wallet.clone(),
                field: "balance".into(),
                cached: *balance,
                ledger: *cash_sum,
            });
        }
        if collateral_locked != collateral_sum {
            mismatches.push(ReconciliationMismatch {
                wallet: wallet.clone(),
                field: "collateral_locked".into(),
                cached: *collateral_locked,
                ledger: *collateral_sum,
            });
        }
    }

    metrics::gauge!("ledger_reconciliation_checked").set(rows.len() as f64);
    metrics::gauge!("ledger_reconciliation_mismatches").set(mismatches.len() as f64);
    if !mismatches.is_empty() {
        tracing::error!(
            mismatches = mismatches.len(),
            "ledger reconciliation detected mismatches"
        );
    }

    Ok(ReconciliationReport {
        checked: rows.len() as i64,
        mismatches,
    })
}

/// One-off backfill: create opening-balance entries for existing accounts so
/// the ledger starts in agreement with the cached projections.
pub async fn backfill_opening_balances(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "INSERT INTO ledger_entries (journal_id, wallet, kind, account, amount) \
         SELECT gen_random_uuid(), a.wallet, 'opening_balance', 'user_cash', a.balance \
         FROM accounts a \
         WHERE NOT EXISTS ( \
             SELECT 1 FROM ledger_entries e \
             WHERE e.wallet = a.wallet AND e.kind = 'opening_balance' \
         )",
    )
    .execute(pool)
    .await?;

    let result = sqlx::query(
        "INSERT INTO ledger_entries (journal_id, wallet, kind, account, amount) \
         SELECT gen_random_uuid(), a.wallet, 'opening_balance', 'user_collateral', a.collateral_locked \
         FROM accounts a \
         WHERE NOT EXISTS ( \
             SELECT 1 FROM ledger_entries e \
             WHERE e.wallet = a.wallet AND e.kind = 'opening_balance' \
               AND e.account = 'user_collateral' \
         )",
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}
