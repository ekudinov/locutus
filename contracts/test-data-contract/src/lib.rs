use locutus_stdlib::prelude::*;

struct Contract;

#[contract]
impl ContractInterface for Contract {
    fn validate_state(_parameters: Parameters<'static>, state: State<'static>) -> bool {
        true
    }

    fn validate_delta(_parameters: Parameters<'static>, delta: StateDelta<'static>) -> bool {
        true
    }

    fn update_state(
        _parameters: Parameters<'static>,
        mut state: State<'static>,
        delta: StateDelta<'static>,
    ) -> Result<UpdateModification, ContractError> {
        Ok(UpdateModification::ValidUpdate(state))
    }

    fn summarize_state(
        _parameters: Parameters<'static>,
        state: State<'static>,
    ) -> StateSummary<'static> {
        let state = state.as_ref();
        // eprintln!("state: {state:?}");
        // eprintln!("summary: {:?}", &state[0..1]);
        StateSummary::from(state[0..1].to_vec())
    }

    fn get_state_delta(
        _parameters: Parameters<'static>,
        state: State<'static>,
        summary: StateSummary<'static>,
    ) -> StateDelta<'static> {
        StateDelta::from((&state).to_vec())
    }

    fn update_state_from_summary(
        _parameters: Parameters<'static>,
        mut state: State<'static>,
        summary: StateSummary<'static>,
    ) -> Result<UpdateModification, ContractError> {
        Ok(UpdateModification::ValidUpdate(State::from(
            state.to_vec(),
        )))
    }
}
