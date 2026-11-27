use super::super::{Live, Place};

#[derive(Clone, PartialEq, Eq)]
pub(in crate::call_order::result_use) struct Origin {
    pub(in crate::call_order::result_use) call: String,
    pub(in crate::call_order::result_use) start: usize,
    pub(in crate::call_order::result_use) end: usize,
    pub(in crate::call_order::result_use) component: Option<usize>,
    pub(in crate::call_order::result_use) line: u32,
    pub(in crate::call_order::result_use) path: Option<String>,
    pub(in crate::call_order::result_use) calls: Vec<(usize, usize)>,
}

pub(in crate::call_order::result_use) enum Value {
    Source(Origin),
    Copy(Place),
    Aggregate(Vec<Value>),
    Other,
}

pub(in crate::call_order::result_use) struct Write {
    pub(in crate::call_order::result_use) target: Option<Place>,
    pub(in crate::call_order::result_use) value: Value,
}

pub(in crate::call_order::result_use) enum Effect {
    Read(Place),
    OpaqueRead(Place),
    OpaqueOnly(Vec<Effect>),
    ObservedOnly(Vec<Effect>),
    Write(Vec<Write>),
    Invoke(usize, Demand),
    ApplyHelper(Box<HelperCall>),
}

pub(in crate::call_order::result_use) struct HelperCall {
    pub(in crate::call_order::result_use) arguments: Vec<Option<Place>>,
    pub(in crate::call_order::result_use) demand: Demand,
    pub(in crate::call_order::result_use) when_discarded: Vec<bool>,
    pub(in crate::call_order::result_use) when_observed: Vec<bool>,
    pub(in crate::call_order::result_use) unobserved_discarded: Vec<Option<bool>>,
    pub(in crate::call_order::result_use) unobserved_observed: Vec<Option<bool>>,
    pub(in crate::call_order::result_use) guard: Option<crate::cfg::IdentityOperand>,
    pub(in crate::call_order::result_use) conditional_discarded: Option<super::super::ConditionalParameters>,
    pub(in crate::call_order::result_use) conditional_observed: Option<super::super::ConditionalParameters>,
    pub(in crate::call_order::result_use) origins_discarded: Vec<(Origin, bool)>,
    pub(in crate::call_order::result_use) origins_observed: Vec<(Origin, bool)>,
    pub(in crate::call_order::result_use) opaque_discarded: Vec<bool>,
    pub(in crate::call_order::result_use) opaque_observed: Vec<bool>,
    pub(in crate::call_order::result_use) opaque_origins_discarded: Vec<Origin>,
    pub(in crate::call_order::result_use) opaque_origins_observed: Vec<Origin>,
}

#[derive(Clone, Copy)]
pub(in crate::call_order::result_use) enum Demand {
    Discarded,
    Observed,
    Slot(Place),
}

impl Demand {
    pub(in crate::call_order::result_use) fn observed(self, live: &mut Live) -> bool {
        match self {
            Self::Discarded => false,
            Self::Observed => true,
            Self::Slot(slot) => live.remove(&slot),
        }
    }
}
