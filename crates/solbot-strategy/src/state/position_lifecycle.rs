use anyhow::{bail, Result};
use solbot_common::types::position::{Position, PositionStatus};
use sqlx::SqlitePool;
use uuid::Uuid;

/// Valid state transitions for the position lifecycle.
///
/// Based on ТЗ 5.3:
/// - `Open → TP1_filled → TP2_filled → TP3_filled → Closed` (TP ladder path)
/// - `Open → Closing → Closed` (SL/trailing path)
/// - `Open → stuck_manual_intervention_required` (error path)
/// - `Closing → Closed` (finalization)
/// - `stuck_manual_intervention_required → Closed` (manual resolution)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// Initial position creation (Opening → Open)
    OpeningToOpen,
    /// TP1 filled (Open → Tp1Filled)
    OpenToTp1,
    /// TP2 filled (Tp1Filled → Tp2Filled)
    Tp1ToTp2,
    /// TP3 filled (Tp2Filled → Tp3Filled)
    Tp2ToTp3,
    /// Moon bag closed (Tp3Filled → Closed)
    Tp3ToClosed,
    /// Stop loss or trailing stop triggered (Open → Closing)
    OpenToClosing,
    /// Stop loss or trailing stop triggered after TP1 (Tp1Filled → Closing)
    Tp1ToClosing,
    /// Stop loss or trailing stop triggered after TP2 (Tp2Filled → Closing)
    Tp2ToClosing,
    /// Moon bag closed via trailing (Tp3Filled → Closing)
    Tp3ToClosing,
    /// Finalization of closing (Closing → Closed)
    ClosingToClosed,
    /// Manual intervention required (any active state → StuckManualInterventionRequired)
    ToStuck,
    /// Manual resolution (StuckManualInterventionRequired → Closed)
    StuckToClosed,
}

impl Transition {
    /// Get the from status for this transition.
    pub fn from_status(&self) -> PositionStatus {
        match self {
            Transition::OpeningToOpen => PositionStatus::Opening,
            Transition::OpenToTp1 => PositionStatus::Open,
            Transition::Tp1ToTp2 => PositionStatus::Tp1Filled,
            Transition::Tp2ToTp3 => PositionStatus::Tp2Filled,
            Transition::Tp3ToClosed => PositionStatus::Tp3Filled,
            Transition::OpenToClosing => PositionStatus::Open,
            Transition::Tp1ToClosing => PositionStatus::Tp1Filled,
            Transition::Tp2ToClosing => PositionStatus::Tp2Filled,
            Transition::Tp3ToClosing => PositionStatus::Tp3Filled,
            Transition::ClosingToClosed => PositionStatus::Closing,
            Transition::ToStuck => PositionStatus::Open, // Can be any active state
            Transition::StuckToClosed => PositionStatus::StuckManualInterventionRequired,
        }
    }

    /// Get the to status for this transition.
    pub fn to_status(&self) -> PositionStatus {
        match self {
            Transition::OpeningToOpen => PositionStatus::Open,
            Transition::OpenToTp1 => PositionStatus::Tp1Filled,
            Transition::Tp1ToTp2 => PositionStatus::Tp2Filled,
            Transition::Tp2ToTp3 => PositionStatus::Tp3Filled,
            Transition::Tp3ToClosed => PositionStatus::Closed,
            Transition::OpenToClosing => PositionStatus::Closing,
            Transition::Tp1ToClosing => PositionStatus::Closing,
            Transition::Tp2ToClosing => PositionStatus::Closing,
            Transition::Tp3ToClosing => PositionStatus::Closing,
            Transition::ClosingToClosed => PositionStatus::Closed,
            Transition::ToStuck => PositionStatus::StuckManualInterventionRequired,
            Transition::StuckToClosed => PositionStatus::Closed,
        }
    }

    /// Get a human-readable reason for this transition.
    pub fn reason(&self) -> &'static str {
        match self {
            Transition::OpeningToOpen => "Position successfully opened",
            Transition::OpenToTp1 => "TP1 (+50%) filled",
            Transition::Tp1ToTp2 => "TP2 (+100%) filled",
            Transition::Tp2ToTp3 => "TP3 (+200%) filled",
            Transition::Tp3ToClosed => "Moon bag closed",
            Transition::OpenToClosing => "Stop loss or trailing stop triggered",
            Transition::Tp1ToClosing => "Stop loss or trailing stop triggered after TP1",
            Transition::Tp2ToClosing => "Stop loss or trailing stop triggered after TP2",
            Transition::Tp3ToClosing => "Trailing stop triggered on moon bag",
            Transition::ClosingToClosed => "Position closed successfully",
            Transition::ToStuck => "Manual intervention required",
            Transition::StuckToClosed => "Manually closed",
        }
    }
}

/// Determine the valid transition between two statuses.
///
/// Returns `None` if the transition is invalid.
pub fn get_valid_transition(from: &PositionStatus, to: &PositionStatus) -> Option<Transition> {
    match (from, to) {
        (PositionStatus::Opening, PositionStatus::Open) => Some(Transition::OpeningToOpen),
        (PositionStatus::Open, PositionStatus::Tp1Filled) => Some(Transition::OpenToTp1),
        (PositionStatus::Tp1Filled, PositionStatus::Tp2Filled) => Some(Transition::Tp1ToTp2),
        (PositionStatus::Tp2Filled, PositionStatus::Tp3Filled) => Some(Transition::Tp2ToTp3),
        (PositionStatus::Tp3Filled, PositionStatus::Closed) => Some(Transition::Tp3ToClosed),
        (PositionStatus::Open, PositionStatus::Closing) => Some(Transition::OpenToClosing),
        (PositionStatus::Tp1Filled, PositionStatus::Closing) => Some(Transition::Tp1ToClosing),
        (PositionStatus::Tp2Filled, PositionStatus::Closing) => Some(Transition::Tp2ToClosing),
        (PositionStatus::Tp3Filled, PositionStatus::Closing) => Some(Transition::Tp3ToClosing),
        (PositionStatus::Closing, PositionStatus::Closed) => Some(Transition::ClosingToClosed),
        (PositionStatus::Open, PositionStatus::StuckManualInterventionRequired) => Some(Transition::ToStuck),
        (PositionStatus::Tp1Filled, PositionStatus::StuckManualInterventionRequired) => Some(Transition::ToStuck),
        (PositionStatus::Tp2Filled, PositionStatus::StuckManualInterventionRequired) => Some(Transition::ToStuck),
        (PositionStatus::Tp3Filled, PositionStatus::StuckManualInterventionRequired) => Some(Transition::ToStuck),
        (PositionStatus::Closing, PositionStatus::StuckManualInterventionRequired) => Some(Transition::ToStuck),
        (PositionStatus::StuckManualInterventionRequired, PositionStatus::Closed) => Some(Transition::StuckToClosed),
        _ => None,
    }
}

/// Check if a transition is valid.
pub fn is_valid_transition(from: &PositionStatus, to: &PositionStatus) -> bool {
    get_valid_transition(from, to).is_some()
}

/// Position lifecycle manager.
///
/// Handles state transitions with validation, persistence, and event emission.
pub struct PositionLifecycle {
    pool: SqlitePool,
}

impl PositionLifecycle {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Transition a position to a new status.
    ///
    /// This method:
    /// 1. Validates the transition
    /// 2. Logs the transition to SQLite
    /// 3. Updates the position status
    /// 4. Returns the transition for event emission
    ///
    /// # Arguments
    /// - `position`: Current position state
    /// - `new_status`: Target status
    /// - `tx_signature`: Optional transaction signature for the transition
    ///
    /// # Returns
    /// - `Ok(transition)` if successful
    /// - `Err` if the transition is invalid or database operation fails
    pub async fn transition(
        &self,
        position: &Position,
        new_status: PositionStatus,
        tx_signature: Option<String>,
    ) -> Result<Transition> {
        let transition = get_valid_transition(&position.status, &new_status)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Invalid transition from {:?} to {:?}",
                    position.status,
                    new_status
                )
            })?;

        // Log the transition to state_transitions table
        let transition_id = Uuid::new_v4().to_string();
        let from_status = crate::state::positions_repo::status_to_string(&position.status);
        let to_status = crate::state::positions_repo::status_to_string(&new_status);
        let reason = transition.reason();
        let created_at = chrono::Utc::now().timestamp() as i64;

        sqlx::query(
            r#"
            INSERT INTO state_transitions (id, position_id, from_status, to_status, reason, tx_signature, created_at_utc)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(&transition_id)
        .bind(&position.position_id)
        .bind(from_status)
        .bind(to_status)
        .bind(reason)
        .bind(&tx_signature)
        .bind(created_at)
        .execute(&self.pool)
        .await?;

        tracing::info!(
            position_id = %position.position_id,
            from = ?position.status,
            to = ?new_status,
            reason = %reason,
            tx_signature = ?tx_signature,
            "Position state transition logged"
        );

        Ok(transition)
    }

    /// Transition a position with a custom reason (for ToStuck transitions).
    ///
    /// This variant allows specifying a custom reason for transitions that
    /// don't have a standard reason (e.g., manual intervention with specific error).
    pub async fn transition_with_reason(
        &self,
        position: &Position,
        new_status: PositionStatus,
        custom_reason: String,
        tx_signature: Option<String>,
    ) -> Result<()> {
        if !is_valid_transition(&position.status, &new_status) {
            bail!(
                "Invalid transition from {:?} to {:?}",
                position.status,
                new_status
            );
        }

        let transition_id = Uuid::new_v4().to_string();
        let from_status = crate::state::positions_repo::status_to_string(&position.status);
        let to_status = crate::state::positions_repo::status_to_string(&new_status);
        let created_at = chrono::Utc::now().timestamp() as i64;

        sqlx::query(
            r#"
            INSERT INTO state_transitions (id, position_id, from_status, to_status, reason, tx_signature, created_at_utc)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(&transition_id)
        .bind(&position.position_id)
        .bind(from_status)
        .bind(to_status)
        .bind(&custom_reason)
        .bind(&tx_signature)
        .bind(created_at)
        .execute(&self.pool)
        .await?;

        tracing::info!(
            position_id = %position.position_id,
            from = ?position.status,
            to = ?new_status,
            reason = %custom_reason,
            tx_signature = ?tx_signature,
            "Position state transition logged with custom reason"
        );

        Ok(())
    }

    /// Get the transition history for a position.
    pub async fn get_transition_history(&self, position_id: &str) -> Result<Vec<StateTransition>> {
        let rows = sqlx::query_as::<_, (String, String, String, String, String, Option<String>, i64)>(
            r#"
            SELECT id, position_id, from_status, to_status, reason, tx_signature, created_at_utc
            FROM state_transitions
            WHERE position_id = ?
            ORDER BY created_at_utc ASC
            "#,
        )
        .bind(position_id)
        .fetch_all(&self.pool)
        .await?;

        let transitions = rows
            .into_iter()
            .map(|(id, position_id, from_status, to_status, reason, tx_signature, created_at)| {
                StateTransition {
                    id,
                    position_id,
                    from_status,
                    to_status,
                    reason,
                    tx_signature,
                    created_at_utc: created_at as u64,
                }
            })
            .collect();

        Ok(transitions)
    }
}

/// A recorded state transition from the database.
#[derive(Debug, Clone)]
pub struct StateTransition {
    pub id: String,
    pub position_id: String,
    pub from_status: String,
    pub to_status: String,
    pub reason: String,
    pub tx_signature: Option<String>,
    pub created_at_utc: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_tp_ladder_transitions() {
        assert!(is_valid_transition(&PositionStatus::Open, &PositionStatus::Tp1Filled));
        assert!(is_valid_transition(&PositionStatus::Tp1Filled, &PositionStatus::Tp2Filled));
        assert!(is_valid_transition(&PositionStatus::Tp2Filled, &PositionStatus::Tp3Filled));
        assert!(is_valid_transition(&PositionStatus::Tp3Filled, &PositionStatus::Closed));
    }

    #[test]
    fn test_valid_sl_trailing_transitions() {
        assert!(is_valid_transition(&PositionStatus::Open, &PositionStatus::Closing));
        assert!(is_valid_transition(&PositionStatus::Tp1Filled, &PositionStatus::Closing));
        assert!(is_valid_transition(&PositionStatus::Tp2Filled, &PositionStatus::Closing));
        assert!(is_valid_transition(&PositionStatus::Tp3Filled, &PositionStatus::Closing));
        assert!(is_valid_transition(&PositionStatus::Closing, &PositionStatus::Closed));
    }

    #[test]
    fn test_valid_stuck_transitions() {
        assert!(is_valid_transition(&PositionStatus::Open, &PositionStatus::StuckManualInterventionRequired));
        assert!(is_valid_transition(&PositionStatus::Tp1Filled, &PositionStatus::StuckManualInterventionRequired));
        assert!(is_valid_transition(&PositionStatus::Closing, &PositionStatus::StuckManualInterventionRequired));
        assert!(is_valid_transition(&PositionStatus::StuckManualInterventionRequired, &PositionStatus::Closed));
    }

    #[test]
    fn test_invalid_transitions() {
        // Can't go backwards
        assert!(!is_valid_transition(&PositionStatus::Tp1Filled, &PositionStatus::Open));
        assert!(!is_valid_transition(&PositionStatus::Closed, &PositionStatus::Open));
        
        // Can't skip TP levels
        assert!(!is_valid_transition(&PositionStatus::Open, &PositionStatus::Tp2Filled));
        assert!(!is_valid_transition(&PositionStatus::Open, &PositionStatus::Tp3Filled));
        
        // Can't go from stuck back to active
        assert!(!is_valid_transition(&PositionStatus::StuckManualInterventionRequired, &PositionStatus::Open));
    }

    #[test]
    fn test_transition_reasons() {
        assert_eq!(Transition::OpenToTp1.reason(), "TP1 (+50%) filled");
        assert_eq!(Transition::Tp1ToTp2.reason(), "TP2 (+100%) filled");
        assert_eq!(Transition::Tp2ToTp3.reason(), "TP3 (+200%) filled");
        assert_eq!(Transition::OpenToClosing.reason(), "Stop loss or trailing stop triggered");
        assert_eq!(Transition::ToStuck.reason(), "Manual intervention required");
    }

    #[test]
    fn test_get_valid_transition() {
        assert_eq!(
            get_valid_transition(&PositionStatus::Open, &PositionStatus::Tp1Filled),
            Some(Transition::OpenToTp1)
        );
        assert_eq!(
            get_valid_transition(&PositionStatus::Open, &PositionStatus::Closed),
            None
        );
    }

    #[test]
    fn test_transition_from_to_status() {
        let transition = Transition::OpenToTp1;
        assert_eq!(transition.from_status(), PositionStatus::Open);
        assert_eq!(transition.to_status(), PositionStatus::Tp1Filled);

        let transition = Transition::Tp1ToClosing;
        assert_eq!(transition.from_status(), PositionStatus::Tp1Filled);
        assert_eq!(transition.to_status(), PositionStatus::Closing);
    }

    #[test]
    fn test_all_transitions_have_valid_from_to() {
        let transitions = [
            Transition::OpeningToOpen,
            Transition::OpenToTp1,
            Transition::Tp1ToTp2,
            Transition::Tp2ToTp3,
            Transition::Tp3ToClosed,
            Transition::OpenToClosing,
            Transition::Tp1ToClosing,
            Transition::Tp2ToClosing,
            Transition::Tp3ToClosing,
            Transition::ClosingToClosed,
            Transition::ToStuck,
            Transition::StuckToClosed,
        ];

        for transition in transitions {
            let from = transition.from_status();
            let to = transition.to_status();
            assert_eq!(
                get_valid_transition(&from, &to),
                Some(transition),
                "Transition {:?} should be valid from {:?} to {:?}",
                transition,
                from,
                to
            );
        }
    }

    #[test]
    fn test_opening_to_open_transition() {
        assert!(is_valid_transition(&PositionStatus::Opening, &PositionStatus::Open));
        assert_eq!(
            get_valid_transition(&PositionStatus::Opening, &PositionStatus::Open),
            Some(Transition::OpeningToOpen)
        );
    }

    #[test]
    fn test_cannot_transition_from_closed() {
        // Once closed, no further transitions allowed
        assert!(!is_valid_transition(&PositionStatus::Closed, &PositionStatus::Open));
        assert!(!is_valid_transition(&PositionStatus::Closed, &PositionStatus::Closing));
        assert!(!is_valid_transition(&PositionStatus::Closed, &PositionStatus::Tp1Filled));
    }

    #[test]
    fn test_cannot_transition_from_stuck_except_to_closed() {
        assert!(is_valid_transition(&PositionStatus::StuckManualInterventionRequired, &PositionStatus::Closed));
        assert!(!is_valid_transition(&PositionStatus::StuckManualInterventionRequired, &PositionStatus::Open));
        assert!(!is_valid_transition(&PositionStatus::StuckManualInterventionRequired, &PositionStatus::Closing));
    }

    #[test]
    fn test_tp3_to_closed_is_valid() {
        assert!(is_valid_transition(&PositionStatus::Tp3Filled, &PositionStatus::Closed));
        assert_eq!(
            get_valid_transition(&PositionStatus::Tp3Filled, &PositionStatus::Closed),
            Some(Transition::Tp3ToClosed)
        );
    }

    #[test]
    fn test_closing_to_closed_only_path() {
        assert!(is_valid_transition(&PositionStatus::Closing, &PositionStatus::Closed));
        assert!(!is_valid_transition(&PositionStatus::Closing, &PositionStatus::Open));
        assert!(!is_valid_transition(&PositionStatus::Closing, &PositionStatus::Tp1Filled));
    }

    #[test]
    fn test_all_active_states_can_go_to_stuck() {
        assert!(is_valid_transition(&PositionStatus::Open, &PositionStatus::StuckManualInterventionRequired));
        assert!(is_valid_transition(&PositionStatus::Tp1Filled, &PositionStatus::StuckManualInterventionRequired));
        assert!(is_valid_transition(&PositionStatus::Tp2Filled, &PositionStatus::StuckManualInterventionRequired));
        assert!(is_valid_transition(&PositionStatus::Tp3Filled, &PositionStatus::StuckManualInterventionRequired));
        assert!(is_valid_transition(&PositionStatus::Closing, &PositionStatus::StuckManualInterventionRequired));
    }

    #[test]
    fn test_transition_variants_cover_all_paths() {
        // TP ladder path
        assert!(matches!(get_valid_transition(&PositionStatus::Open, &PositionStatus::Tp1Filled), Some(Transition::OpenToTp1)));
        assert!(matches!(get_valid_transition(&PositionStatus::Tp1Filled, &PositionStatus::Tp2Filled), Some(Transition::Tp1ToTp2)));
        assert!(matches!(get_valid_transition(&PositionStatus::Tp2Filled, &PositionStatus::Tp3Filled), Some(Transition::Tp2ToTp3)));
        assert!(matches!(get_valid_transition(&PositionStatus::Tp3Filled, &PositionStatus::Closed), Some(Transition::Tp3ToClosed)));

        // SL/trailing path
        assert!(matches!(get_valid_transition(&PositionStatus::Open, &PositionStatus::Closing), Some(Transition::OpenToClosing)));
        assert!(matches!(get_valid_transition(&PositionStatus::Closing, &PositionStatus::Closed), Some(Transition::ClosingToClosed)));

        // Stuck path
        assert!(matches!(get_valid_transition(&PositionStatus::Open, &PositionStatus::StuckManualInterventionRequired), Some(Transition::ToStuck)));
        assert!(matches!(get_valid_transition(&PositionStatus::StuckManualInterventionRequired, &PositionStatus::Closed), Some(Transition::StuckToClosed)));
    }
}
