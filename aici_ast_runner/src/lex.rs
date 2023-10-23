use aici_abi::wprintln;
use regex_automata::{
    dfa::{dense, Automaton},
    util::{primitives::StateID, syntax},
};
use rustc_hash::FxHashMap;
use std::{hash::Hash, vec};
use vob::{vob, Vob};

type PatIdx = usize;

const LOG_LEXER: bool = false;

#[derive(Debug, Clone, Hash, PartialEq, Eq, Copy)]
pub struct VobIdx(usize);

impl VobIdx {
    pub fn is_zero(&self) -> bool {
        self.0 == 0
    }
}

pub struct VobSet {
    vobs: Vec<Vob>,
    by_vob: FxHashMap<Vob, VobIdx>,
    non_empty: Vob,
}

impl VobSet {
    pub fn new() -> Self {
        VobSet {
            vobs: Vec::new(),
            by_vob: FxHashMap::default(),
            non_empty: Vob::new(),
        }
    }

    pub fn get(&mut self, vob: &Vob) -> VobIdx {
        if let Some(idx) = self.by_vob.get(vob) {
            return *idx;
        }
        let len = self.vobs.len();
        if len == 0 && !vob_is_zero(vob) {
            panic!("first vob must be empty");
        }
        let idx = VobIdx(len);
        self.vobs.push(vob.clone());
        self.by_vob.insert(vob.clone(), idx);
        idx
    }

    pub fn and_is_zero(&self, a: VobIdx, b: VobIdx) -> bool {
        vob_and_is_zero(&self.vobs[a.0], &self.vobs[b.0])
        // !self.non_empty[a.0 * self.vobs.len() + b.0]
    }

    pub fn pre_compute(&mut self) {
        let l = self.vobs.len();
        self.non_empty.resize(l * l, false);
        for x in 0..self.vobs.len() {
            for y in 0..=x {
                if !vob_and_is_zero(&self.vobs[x], &self.vobs[y]) {
                    self.non_empty.set(x * l + y, true);
                    self.non_empty.set(y * l + x, true);
                }
            }
        }
        wprintln!(
            "vobset: {} vobs, {} nonempty",
            self.vobs.len(),
            self.non_empty.len()
        );
    }
}

pub struct Lexer {
    dfa: dense::DFA<Vec<u32>>,
    pub skip_patterns: Vob,
    pub friendly_pattern_names: Vec<String>,
    possible_by_state: FxHashMap<StateID, VobIdx>,
    initial: StateID,
    pub file_start: StateID,
    vobidx_by_state_off: Vec<VobIdx>,
}

impl Lexer {
    pub fn from(
        patterns: Vec<String>,
        skip_patterns: Vob,
        friendly_pattern_names: Vec<String>,
        vobset: &mut VobSet,
    ) -> Self {
        let dfa = dense::Builder::new()
            .configure(
                dense::Config::new()
                    .start_kind(regex_automata::dfa::StartKind::Anchored)
                    .match_kind(regex_automata::MatchKind::All),
            )
            .syntax(syntax::Config::new().unicode(false).utf8(false))
            .build_many(&patterns)
            .unwrap();

        wprintln!(
            "dfa: {} bytes, {} patterns",
            dfa.memory_usage(),
            patterns.len(),
        );
        if false {
            for p in &patterns {
                wprintln!("  {}", p)
            }
        }

        let anch = regex_automata::Anchored::Yes;

        let mut incoming = FxHashMap::default();
        let initial = dfa.universal_start_state(anch).unwrap();
        let mut todo = vec![initial];
        incoming.insert(initial, Vec::new());
        while todo.len() > 0 {
            let s = todo.pop().unwrap();
            for b in 0..=255 {
                let s2 = dfa.next_state(s, b);
                if !incoming.contains_key(&s2) {
                    todo.push(s2);
                    incoming.insert(s2, Vec::new());
                }
                incoming.get_mut(&s2).unwrap().push(s);
            }
        }

        let states = incoming.keys().map(|x| *x).collect::<Vec<_>>();
        let mut tokenset_by_state = FxHashMap::default();

        for s in &states {
            let mut v = vob![false; patterns.len()];
            let s2 = dfa.next_eoi_state(*s);
            if dfa.is_match_state(s2) {
                for idx in 0..dfa.match_len(s2) {
                    let idx = dfa.match_pattern(s2, idx).as_usize();
                    v.set(idx, true);
                    if LOG_LEXER {
                        wprintln!("  match: {:?} {}", *s, patterns[idx])
                    }
                }
            }
            tokenset_by_state.insert(*s, v);
        }

        loop {
            let mut num_set = 0;

            for s in &states {
                let ours = tokenset_by_state.get(s).unwrap().clone();
                for o in &incoming[s] {
                    let theirs = tokenset_by_state.get(o).unwrap();
                    let mut tmp = ours.clone();
                    tmp |= theirs;
                    if tmp != *theirs {
                        num_set += 1;
                        tokenset_by_state.insert(*o, tmp);
                    }
                }
            }

            if LOG_LEXER {
                wprintln!("iter {} {}", num_set, states.len());
            }
            if num_set == 0 {
                break;
            }
        }

        let mut states_idx = states.iter().map(|x| x.as_usize()).collect::<Vec<_>>();
        states_idx.sort();

        let shift = dfa.stride2();
        let mut vobidx_by_state_off =
            vec![VobIdx(0); 1 + (states_idx.iter().max().unwrap() >> shift)];
        for (k, v) in tokenset_by_state.iter() {
            vobidx_by_state_off[k.as_usize() >> shift] = vobset.get(v);
        }

        // pretend we've just seen a newline at the beginning of the file
        // TODO: this should be configurable
        let file_start = dfa.next_state(initial, b'\n');
        wprintln!(
            "initial: {:?} {:?}; {} states",
            initial,
            file_start,
            states.len()
        );

        let lex = Lexer {
            dfa,
            skip_patterns,
            friendly_pattern_names,
            vobidx_by_state_off,
            possible_by_state: tokenset_by_state
                .iter()
                .map(|(k, v)| (k.clone(), vobset.get(v)))
                .collect(),
            initial,
            file_start,
        };

        if LOG_LEXER {
            for s in &states {
                if lex.is_dead(*s) {
                    wprintln!("dead: {:?} {}", s, lex.dfa.is_dead_state(*s));
                }
            }

            wprintln!("possible_tokens: {:#?}", lex.possible_by_state);
        }

        lex
    }

    fn is_dead(&self, state: StateID) -> bool {
        self.possible_tokens(state).is_zero()
    }

    fn possible_tokens(&self, state: StateID) -> VobIdx {
        self.vobidx_by_state_off[state.as_usize() >> self.dfa.stride2()]
        // *self.possible_by_state.get(&state).unwrap()
    }

    fn get_token(&self, state: StateID) -> Option<PatIdx> {
        if !self.dfa.is_match_state(state) {
            return None;
        }

        // we take the first token that matched
        // (eg., "while" will match both keyword and identifier, but keyword is first)
        let pat_idx = (0..self.dfa.match_len(state))
            .map(|idx| self.dfa.match_pattern(state, idx).as_usize())
            .min()
            .unwrap();

        if LOG_LEXER {
            wprintln!("token: {}", self.friendly_pattern_names[pat_idx]);
        }

        Some(pat_idx)
    }

    #[inline(always)]
    pub fn advance(
        &self,
        prev: StateID,
        byte: Option<u8>,
    ) -> Option<(StateID, VobIdx, Option<PatIdx>)> {
        let dfa = &self.dfa;
        if let Some(byte) = byte {
            let state = dfa.next_state(prev, byte);
            if LOG_LEXER {
                wprintln!(
                    "lex: {:?} -{:?}-> {:?} d={}",
                    prev,
                    byte as char,
                    state,
                    self.is_dead(state),
                );
            }
            let v = self.possible_tokens(state);
            if v.is_zero() {
                let final_state = dfa.next_eoi_state(prev);
                // if final_state is a match state, find the token that matched
                let tok = self.get_token(final_state);
                if tok.is_none() {
                    None
                } else {
                    let state = dfa.next_state(self.initial, byte);
                    if LOG_LEXER {
                        wprintln!("lex0: {:?} -{:?}-> {:?}", self.initial, byte as char, state);
                    }
                    Some((state, self.possible_tokens(state), tok))
                }
            } else {
                Some((state, v, None))
            }
        } else {
            let final_state = dfa.next_eoi_state(prev);
            let tok = self.get_token(final_state);
            if tok.is_none() {
                None
            } else {
                Some((self.initial, self.possible_tokens(self.initial), tok))
            }
        }
    }
}

fn vob_and_is_zero(a: &Vob, b: &Vob) -> bool {
    debug_assert!(a.len() == b.len());
    for (a, b) in a.iter_storage().zip(b.iter_storage()) {
        if a & b != 0 {
            return false;
        }
    }
    return true;
}

fn vob_is_zero(v: &Vob) -> bool {
    for b in v.iter_storage() {
        if b != 0 {
            return false;
        }
    }
    true
}
