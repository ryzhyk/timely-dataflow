use std::cmp::Ordering;
use std::default::Default;
use core::fmt::Show;

use std::rc::Rc;
use std::cell::RefCell;

use progress::frontier::{MutableAntichain, Antichain};
use progress::{Timestamp, PathSummary, Graph, Scope};
use progress::subgraph::Source::{GraphInput, ScopeOutput};
use progress::subgraph::Target::{GraphOutput, ScopeInput};
use progress::subgraph::Location::{SourceLoc, TargetLoc};

use progress::subgraph::Summary::{Local, Outer};
use progress::count_map::CountMap;

#[deriving(Eq, PartialEq, Hash, Copy, Clone, Show)]
pub enum Source
{
    GraphInput(uint),           // from outer scope
    ScopeOutput(uint, uint),    // (scope, port) may have interesting connectivity
}

#[deriving(Eq, PartialEq, Hash, Copy, Clone, Show)]
pub enum Target
{
    GraphOutput(uint),          // to outer scope
    ScopeInput(uint, uint),     // (scope, port) may have interesting connectivity
}

#[deriving(Eq, PartialEq, Hash, Copy, Clone, Show)]
pub enum Location
{
    SourceLoc(Source),
    TargetLoc(Target),
}


impl<TOuter: Timestamp, TInner: Timestamp> Timestamp for (TOuter, TInner) { }

#[deriving(Copy, Clone, Eq, PartialEq, Show)]
pub enum Summary<S, T>
{
    Local(T),    // reachable within scope, after some iterations.
    Outer(S, T), // unreachable within scope, reachable through outer scope and some iterations.
}

impl<S, T: Default> Default for Summary<S, T>
{
    fn default() -> Summary<S, T> { Local(Default::default()) }
}

impl<S:PartialOrd+Copy, T:PartialOrd+Copy> PartialOrd for Summary<S, T>
{
    fn partial_cmp(&self, other: &Summary<S, T>) -> Option<Ordering>
    {
        match *self
        {
            Local(iters) =>
            {
                match *other
                {
                    Local(iters2) => iters.partial_cmp(&iters2),
                    _ => Some(Less),
                }
            },
            Outer(s1, iters) =>
            {
                match *other
                {
                    Outer(s2, iters2) => if s1.eq(&s2) { iters.partial_cmp(&iters2) }
                                         else          { s1.partial_cmp(&s2) },
                    _ => Some(Greater),
                }
            },
        }
    }
}

impl<TOuter, SOuter, TInner, SInner>
PathSummary<(TOuter, TInner)>
for Summary<SOuter, SInner>
where TOuter: Timestamp,
      TInner: Timestamp,
      SOuter: PathSummary<TOuter>,
      SInner: PathSummary<TInner>,
{
    // this makes sense for a total order, but less clear for a partial order.
    fn results_in(&self, src: &(TOuter, TInner)) -> (TOuter, TInner)
    {
        let &(outer, inner) = src;

        match *self
        {
            Local(iters) => (outer, iters.results_in(&inner)),
            Outer(ref summary, iters) => (summary.results_in(&outer), iters.results_in(&Default::default())),
        }
    }
    fn followed_by(&self, other: &Summary<SOuter, SInner>) -> Summary<SOuter, SInner>
    {
        match *self
        {
            Local(iter) =>
            {
                match *other
                {
                    Local(iter2) => Local(iter.followed_by(&iter2)),
                    Outer(_, _) => *other,
                }
            },
            Outer(summary, iter) =>
            {
                match *other
                {
                    Local(iter2) => Outer(summary, iter.followed_by(&iter2)),
                    Outer(sum2, iter2) => Outer(summary.followed_by(&sum2), iter2),
                }
            },
        }
    }
}

#[deriving(Default)]
pub struct SubscopeState<TTime:Timestamp, TSummary>
{
    summary:                Vec<Vec<Antichain<TSummary>>>,  // internal path summaries
    guarantees:             Vec<MutableAntichain<TTime>>,   // guarantee made by parent scope
    capabilities:           Vec<MutableAntichain<TTime>>,   // capabilities retained by scope
    outstanding_messages:   Vec<MutableAntichain<TTime>>,
}

impl<TTime:Timestamp, TSummary> SubscopeState<TTime, TSummary>
{
    pub fn new(inputs: uint, outputs: uint, summary: Vec<Vec<Antichain<TSummary>>>) -> SubscopeState<TTime, TSummary>
    {
        SubscopeState
        {
            summary:                summary,
            guarantees:             Vec::from_fn(inputs, |_| Default::default()),
            capabilities:           Vec::from_fn(outputs, |_| Default::default()),
            outstanding_messages:   Vec::from_fn(inputs, |_| Default::default()),
        }
    }
}

#[deriving(Default)]
pub struct SubscopeBuffers<TTime:Timestamp>
{
    progress:        Vec<Vec<(TTime, i64)>>,
    consumed:        Vec<Vec<(TTime, i64)>>,
    produced:        Vec<Vec<(TTime, i64)>>,

    guarantee_changes:      Vec<Vec<(TTime, i64)>>,
}


impl<TTime:Timestamp> SubscopeBuffers<TTime>
{
    pub fn new(inputs: uint, outputs: uint) -> SubscopeBuffers<TTime>
    {
        SubscopeBuffers
        {
            progress:    Vec::from_fn(outputs, |_| Vec::new()),
            consumed:    Vec::from_fn(inputs, |_| Vec::new()),
            produced:    Vec::from_fn(outputs, |_| Vec::new()),

            guarantee_changes:  Vec::from_fn(inputs, |_| Vec::new()),
        }
    }
}

#[deriving(Default)]
pub struct PointstampCounter<T:Timestamp>
{
    pub source_counts:  Vec<Vec<Vec<(T, i64)>>>,
    pub target_counts:  Vec<Vec<Vec<(T, i64)>>>,
    pub input_counts:   Vec<Vec<(T, i64)>>,

    pub target_pushed:  Vec<Vec<Vec<(T, i64)>>>,
    pub output_pushed:  Vec<Vec<(T, i64)>>,
}

impl<T:Timestamp> PointstampCounter<T>
{
    //#[inline(always)]
    pub fn update(&mut self, location: Location, time: T, value: i64)
    {
        match location
        {
            SourceLoc(source) =>
            {
                match source
                {
                    ScopeOutput(scope, output) => { self.source_counts[scope][output].update(time, value); },
                    GraphInput(input)          => { self.input_counts[input].update(time, value); },
                }
            },
            TargetLoc(target) =>
            {
                if let ScopeInput(scope, input) = target { self.target_counts[scope][input].update(time, value); }
                else                                     { println!("lolwut?"); }
            }
        }
    }

    pub fn clear_pushed(&mut self)
    {
        for vec in self.target_pushed.iter_mut() { for map in vec.iter_mut() { map.clear(); } }
        for map in self.output_pushed.iter_mut() { map.clear(); }
    }
}

#[deriving(Default)]
pub struct Subgraph<TOuter:Timestamp, SOuter, TInner:Timestamp, SInner>
{
    pub name:               String,

    pub index:              uint,

    default_time:           (TOuter, TInner),
    default_summary:        Summary<SOuter, SInner>,

    // inputs and outputs of the scope
    inputs:                 uint,
    outputs:                uint,

    // all edges managed by this subgraph, and counts for outstanding messages on them.
    scope_edges:            Vec<Vec<Vec<Target>>>,  // map from (scope, port) -> Vec<Target> list.
    input_edges:            Vec<Vec<Target>>,       // map from input -> Vec<Target> list.

    // path summaries along internal, external, and arbitrary edges.
    external_summaries:     Vec<Vec<Antichain<SOuter>>>,

    // maps from (scope, output), (scope, input) and (input) to respective Vec<(target, antichain)> lists
    // TODO: sparsify complete_summaries to contain only paths which avoid their target scopes.
    source_summaries:       Vec<Vec<Vec<(Target, Antichain<Summary<SOuter, SInner>>)>>>,
    target_summaries:       Vec<Vec<Vec<(Target, Antichain<Summary<SOuter, SInner>>)>>>,
    input_summaries:        Vec<Vec<(Target, Antichain<Summary<SOuter, SInner>>)>>,

    // state reflicting work in and promises made to external scope.
    external_capability:    Vec<MutableAntichain<TOuter>>,
    external_guarantee:     Vec<MutableAntichain<TOuter>>,

    // all of the subscopes, and their internal_summaries (ss[g][i][o] = ss[g].i_s[i][o])
    subscopes:              Vec<Box<Scope<(TOuter, TInner), Summary<SOuter, SInner>>>>,
    subscope_state:         Vec<SubscopeState<(TOuter, TInner), Summary<SOuter, SInner>>>,
    subscope_buffers:       Vec<SubscopeBuffers<(TOuter, TInner)>>,

    pointstamps:            PointstampCounter<(TOuter, TInner)>,

    input_messages:         Vec<Rc<RefCell<Vec<((TOuter, TInner), i64)>>>>,
}


impl<TOuter, SOuter, TInner, SInner>
Scope<TOuter, SOuter>
for Subgraph<TOuter, SOuter, TInner, SInner>
where TOuter: Timestamp,
      TInner: Timestamp,
      SOuter: PathSummary<TOuter>,
      SInner: PathSummary<TInner>,
{
    fn name(&self) -> String { self.name.clone() }

    fn inputs(&self)  -> uint { self.inputs }
    fn outputs(&self) -> uint { self.outputs }

    // produces (in -> out) summaries using only edges internal to the vertex.
    fn get_internal_summary(&mut self) -> (Vec<Vec<Antichain<SOuter>>>, Vec<Vec<(TOuter, i64)>>)
    {
        // seal subscopes; prepare per-scope state/buffers
        for index in range(0, self.subscopes.len())
        {
            let (summary, work) = self.subscopes[index].get_internal_summary();

            let inputs = self.subscopes[index].inputs();
            let outputs = self.subscopes[index].outputs();

            let mut new_state = SubscopeState::new(inputs, outputs, summary);

            // install initial capabilities
            for output in range(0, outputs)
            {
                new_state.capabilities[output].update_iter_and(work[output].iter().map(|&x|x), |_, _| {});
            }

            self.subscope_state.push(new_state);
            self.subscope_buffers.push(SubscopeBuffers::new(inputs, outputs));

            // initialize storage for vector-based source and target path summaries.
            self.source_summaries.push(Vec::from_fn(outputs, |_| Vec::new()));
            self.target_summaries.push(Vec::from_fn(inputs, |_| Vec::new()));

            self.pointstamps.target_pushed.push(Vec::from_fn(inputs, |_| Default::default()));
            self.pointstamps.target_counts.push(Vec::from_fn(inputs, |_| Default::default()));
            self.pointstamps.source_counts.push(Vec::from_fn(outputs, |_| Default::default()));

            // take capabilities as pointstamps
            for output in range(0, outputs)
            {
                let location = SourceLoc(ScopeOutput(index, output));
                for &time in self.subscope_state[index].capabilities[output].elements.iter()
                {
                    self.pointstamps.update(location, time, 1);
                }
            }
        }

        // initialize space for input -> Vec<(Target, Antichain) mapping.
        self.input_summaries = Vec::from_fn(self.inputs(), |_| Vec::new());

        self.pointstamps.input_counts = Vec::from_fn(self.inputs(), |_| Default::default());
        self.pointstamps.output_pushed = Vec::from_fn(self.outputs(), |_| Default::default());

        self.external_summaries = Vec::from_fn(self.outputs(), |_| Vec::from_fn(self.inputs(), |_| Default::default()));

        // TODO: Explain better.
        self.set_summaries();

        self.push_pointstamps_to_targets();

        // TODO: WTF is this all about? Who wrote this? Me...
        let mut work = Vec::from_fn(self.outputs(), |_| Vec::new());
        for (output, map) in work.iter_mut().enumerate()
        {
            for &(key, val) in self.pointstamps.output_pushed[output].elements().iter()
            {
                map.push((key.val0(), val));
            }
        }

        let mut summaries = Vec::from_fn(self.inputs(), |_| Vec::from_fn(self.outputs(), |_| Antichain::new()));
        for input in range(0, self.inputs())
        {
            for &(target, ref antichain) in self.input_summaries[input].iter()
            {
                if let GraphOutput(output) = target
                {
                    for &summary in antichain.elements.iter()
                    {
                        summaries[input][output].insert(match summary
                        {
                            Local(_)    => Default::default(),
                            Outer(y, _) => y,
                        });
                    };
                }
            }
        }

        self.pointstamps.clear_pushed();

        return (summaries, work);
    }

    // receives (out -> in) summaries using only edges external to the vertex.
    fn set_external_summary(&mut self, summaries: Vec<Vec<Antichain<SOuter>>>, frontier: &Vec<Vec<(TOuter, i64)>>) -> ()
    {
        self.external_summaries = summaries;

        // now sort out complete reachability internally...
        self.set_summaries();

        // change frontier to local times; introduce as pointstamps
        for graph_input in range(0, self.inputs())
        {
            for &(time, val) in frontier[graph_input].iter()
            {
                self.pointstamps.update(SourceLoc(GraphInput(graph_input)), (time, Default::default()), val);
            }
        }

        // identify all capabilities expressed locally
        for scope in range(0, self.subscopes.len())
        {
            for output in range(0, self.subscopes[scope].outputs())
            {
                for &time in self.subscope_state[scope].capabilities[output].elements.iter()
                {
                    self.pointstamps.update(SourceLoc(ScopeOutput(scope, output)), time, 1);
                }
            }
        }

        self.push_pointstamps_to_targets();

        // for each subgraph, compute summaries based on external edges.
        for subscope in range(0, self.subscopes.len())
        {
            // meant to be stashed somewhere, rather than constructed each time.
            let changes = &mut self.subscope_buffers[subscope].guarantee_changes;

            if self.subscopes[subscope].notify_me()
            {
                for input_port in range(0, changes.len())
                {
                    self.subscope_state[subscope]
                        .guarantees[input_port]
                        .update_into_cm(&self.pointstamps.target_pushed[subscope][input_port], &mut changes[input_port]);
                }
            }

            let inputs = self.subscopes[subscope].inputs();
            let outputs = self.subscopes[subscope].outputs();

            let mut summaries = Vec::from_fn(outputs, |_| Vec::from_fn(inputs, |_| Antichain::new()));

            for output in range(0, summaries.len())
            {
                for &(target, ref antichain) in self.source_summaries[subscope][output].iter()
                {
                    if let ScopeInput(target_scope, target_input) = target
                    {
                        if target_scope == subscope { summaries[output][target_input] = antichain.clone()}
                    }
                }
            }

            self.subscopes[subscope].set_external_summary(summaries, changes);
            for change in changes.iter_mut() { change.clear(); }
        }

        self.pointstamps.clear_pushed();
    }

    // information for the scope about progress in the outside world (updates to the input frontiers)
    // important to push this information on to subscopes.
    fn push_external_progress(&mut self, frontier_progress: &Vec<Vec<(TOuter, i64)>>) -> ()
    {
        // transform into pointstamps to use push_progress_to_target().
        for (input, progress) in frontier_progress.iter().enumerate()
        {
            for &(time, val) in progress.iter()
            {
                self.pointstamps.update(SourceLoc(GraphInput(input)), (time, Default::default()), val);
            }
        }

        self.push_pointstamps_to_targets();

        // consider pushing to each nested scope in turn.
        for (index, scope) in self.subscopes.iter_mut().enumerate()
        {
            if scope.notify_me()
            {
                let changes = &mut self.subscope_buffers[index].guarantee_changes;

                for input_port in range(0, changes.len())
                {
                    self.subscope_state[index].guarantees[input_port]
                        .update_into_cm(&self.pointstamps.target_pushed[index][input_port], &mut changes[input_port]);
                }

                // push any changes to the frontier to the subgraph.
                if changes.iter().any(|x| x.len() > 0)
                {
                    scope.push_external_progress(changes);
                    for change in changes.iter_mut() { change.clear(); }
                }
            }
        }

        self.pointstamps.clear_pushed();
    }

    // information from the vertex about its progress (updates to the output frontiers, recv'd and sent message counts)
    fn pull_internal_progress(&mut self, frontier_progress: &mut Vec<Vec<(TOuter, i64)>>,         // to populate
                                         messages_consumed: &mut Vec<Vec<(TOuter, i64)>>,         // to populate
                                         messages_produced: &mut Vec<Vec<(TOuter, i64)>>) -> ()   // to populate
    {
        // Step 1: handle messages introduced through each graph input
        for input in range(0, self.inputs())
        {
            // we'll need this field later on ...
            let pointstamps = &mut self.pointstamps;

            if self.input_messages[input].borrow().len() > 0
            {
                let mut input_message_counts = self.input_messages[input].borrow_mut();
                for &(key, val) in input_message_counts.iter()
                {
                    messages_consumed[input].push((key.val0(), val));
                }

                // push information about messages introduced to adjacent targets.
                for &target in self.input_edges[input].iter()
                {
                    match target
                    {
                        // scopes should know to expect messages.
                        ScopeInput(subgraph, subgraph_input) =>
                        {
                            self.subscope_state[subgraph].outstanding_messages[subgraph_input]
                                .update_iter_and(input_message_counts.iter().map(|&(x,y)| (x,y)), |time, delta|
                                {
                                    pointstamps.update(TargetLoc(target), time, delta);
                                });
                        },
                        // outputs should report messages produced.
                        GraphOutput(graph_output) =>
                        {
                            for &(time, val) in input_message_counts.iter()
                            {
                                messages_produced[graph_output].push((time.val0(), val));
                            }
                        },
                    }
                }

                // clear counts before releasing
                input_message_counts.clear();
            }
        }

        // Step 2: pull_internal_progress from subscopes.
        for (index, scope) in self.subscopes.iter_mut().enumerate()
        {
            // we'll need this field later on ...
            let pointstamps = &mut self.pointstamps;

            let buffers = &mut self.subscope_buffers[index];

            scope.pull_internal_progress(&mut buffers.progress,
                                         &mut buffers.consumed,
                                         &mut buffers.produced);

            for output in range(0, scope.outputs())
            {
                // Step 2a: handle produced messages!
                if buffers.produced[output].len() > 0
                {
                    // for each destination from the source, bump some counts!
                    for &target in self.scope_edges[index][output].iter()
                    {
                        match target
                        {
                            // push messages into antichain, and to a pointstamp if it changes the frontier.
                            ScopeInput(target_scope, target_port) =>
                            {
                                self.subscope_state[target_scope].outstanding_messages[target_port]
                                    .update_iter_and(buffers.produced[output].iter().map(|&x| x), |time, delta|
                                    {
                                        pointstamps.update(TargetLoc(target), time, delta);
                                    });
                            },
                            // indicate messages produced.
                            GraphOutput(graph_output) =>
                            {
                                // do something, um. as part of figuring out the result of the function. :D
                                for &(key, val) in buffers.produced[output].iter()
                                {
                                    messages_produced[graph_output].push((key.val0(), val));
                                }
                            },
                        }
                    }

                    buffers.produced[output].clear();
                }

                // Step 2b: handle progress updates!
                if buffers.progress[output].len() > 0
                {
                    self.subscope_state[index].capabilities[output]
                        .update_iter_and(buffers.progress[output].iter().map(|&x| x), |time, delta|
                        {
                            pointstamps.update(SourceLoc(ScopeOutput(index, output)), time, delta);
                        });

                    buffers.progress[output].clear();
                }
            }

            for input in range(0, scope.inputs())
            {
                // Step 2c: handle consumed messages.
                if buffers.consumed[input].len() > 0
                {
                    //let mut pointstamps = &mut self.pointstamps;
                    self.subscope_state[index].outstanding_messages[input]
                        .update_iter_and(buffers.consumed[input].iter().map(|&(x, y)| (x,-y)), |time, delta|
                        {
                            pointstamps.update(TargetLoc(ScopeInput(index, input)), time, delta);
                        });

                    buffers.consumed[input].clear();
                }
            }
        }

        // holy crap! Now we have a huge pile of updates to various locations in the scope... *pant* *pant*

        // moves self.pointstamps to self.pointstamps.pushed, which are differentiated by target.
        self.push_pointstamps_to_targets();

        // Step 3: push any progress to each target subgraph ...
        for (index, scope) in self.subscopes.iter_mut().enumerate()
        {
            if scope.notify_me()
            {
                let changes = &mut self.subscope_buffers[index].guarantee_changes;

                for input_port in range(0, changes.len())
                {
                    self.subscope_state[index]
                        .guarantees[input_port]
                        .update_into_cm(&self.pointstamps.target_pushed[index][input_port], &mut changes[input_port]);
                }

                // push any changes to the frontier to the subgraph.
                if changes.iter().any(|x| x.len() > 0)
                {
                    scope.push_external_progress(changes);
                    for change in changes.iter_mut() { change.clear(); }
                }
            }
        }

        // Step 4: push progress to each graph output ...
        for output in range(0, self.outputs())
        {
            // prep an iterator which extracts the first field of the time
            let updates = self.pointstamps.output_pushed[output].iter().map(|&(time, val)| (time.val0(), val));
            self.external_capability[output].update_iter_and(updates, |time,val| { frontier_progress[output].update(time, val); });
        }

        // pointstamps should be cleared in push_to_targets()
        self.pointstamps.clear_pushed();
    }
}


impl<TOuter, SOuter, TInner, SInner>
Graph<(TOuter, TInner), Summary<SOuter, SInner>>
for Rc<RefCell<Subgraph<TOuter, SOuter, TInner, SInner>>>
where TOuter: Timestamp,
      TInner: Timestamp,
      SOuter: PathSummary<TOuter>,
      SInner: PathSummary<TInner>,
{
    fn connect(&mut self, source: Source, target: Target)
    {
        self.borrow_mut().connect(source, target);
    }

    fn add_scope(&mut self, scope: Box<Scope<(TOuter, TInner), Summary<SOuter, SInner>>>) -> uint
    {
        let mut borrow = self.borrow_mut();

        borrow.subscopes.push(scope);
        return borrow.subscopes.len() - 1;
    }

    fn as_box(&self) -> Box<Graph<(TOuter, TInner), Summary<SOuter, SInner>>> { box self.clone() }
}

impl<TOuter, SOuter, TInner, SInner>
Subgraph<TOuter, SOuter, TInner, SInner>
where TOuter: Timestamp,
      TInner: Timestamp,
      SOuter: PathSummary<TOuter>,
      SInner: PathSummary<TInner>,
{
    fn push_pointstamps_to_targets(&mut self) -> ()
    {
        // for each scope, do inputs and outputs
        for (index, scope) in self.subscopes.iter().enumerate()
        {
            // for each input of the scope
            for input in range(0, scope.inputs())
            {
                // for each of the updates at ScopeInput(scope, input) ...
                for &(time, value) in self.pointstamps.target_counts[index][input].elements().iter()
                {
                    // for each target it can reach ...
                    for &(target, ref antichain) in self.target_summaries[index][input].iter()
                    {
                        for summary in antichain.elements.iter()
                        {
                            // do stuff.
                            let dest = match target
                            {
                                ScopeInput(scope, input) => &mut self.pointstamps.target_pushed[scope][input],
                                GraphOutput(output)      => &mut self.pointstamps.output_pushed[output],
                            };

                            dest.update(summary.results_in(&time), value);
                        }
                    }
                }

                // erase propagated pointstamp updates
                self.pointstamps.target_counts[index][input].clear();
            }

            // for each output of the scope
            for output in range(0, scope.outputs())
            {
                // for each of the updates at ScopeOutput(scope, output) ...
                for &(time, value) in self.pointstamps.source_counts[index][output].elements().iter()
                {
                    // for each target it can reach ...
                    for &(target, ref antichain) in self.source_summaries[index][output].iter()
                    {
                        for summary in antichain.elements.iter()
                        {
                            // do stuff.
                            let dest = match target
                            {
                                ScopeInput(scope, input) => &mut self.pointstamps.target_pushed[scope][input],
                                GraphOutput(output)      => &mut self.pointstamps.output_pushed[output],
                            };

                            dest.update(summary.results_in(&time), value);
                        }
                    }
                }

                // erase propagated pointstamp updates
                self.pointstamps.source_counts[index][output].clear();
            }
        }

        // for each of the graph inputs
        for input in range(0, self.inputs())
        {
            // for each of the updates at GraphInput(input) ...
            for &(time, value) in self.pointstamps.input_counts[input].iter()
            {
                // for each target it can reach ...
                for &(target, ref antichain) in self.input_summaries[input].iter()
                {
                    for summary in antichain.elements.iter()
                    {
                        // do stuff.
                        let dest = match target
                        {
                            ScopeInput(scope, input) => &mut self.pointstamps.target_pushed[scope][input],
                            GraphOutput(output)      => &mut self.pointstamps.output_pushed[output],
                        };

                        dest.update(summary.results_in(&time), value);
                    }
                }
            }

            // erase propagated pointstamp updates
            self.pointstamps.input_counts[input].clear();
        }
    }

    // Repeatedly takes edges (source, target), finds (target, source') connections,
    // expands based on (source', target') summaries.
    // Only considers targets satisfying the supplied predicate.
    fn set_summaries(&mut self) -> ()
    {
        // load up edges from source outputs
        for scope in range(0, self.subscopes.len())
        {
            for output in range(0, self.subscopes[scope].outputs())
            {
                self.source_summaries[scope][output].clear();
                for &target in self.scope_edges[scope][output].iter()
                {
                    if match target { ScopeInput(t, _) => self.subscopes[t].notify_me(), _ => true }
                    {
                        self.source_summaries[scope][output].push((target, Antichain::from_elem(self.default_summary)));
                    }
                }
            }
        }

        // load up edges from graph inputs
        for input in range(0, self.inputs())
        {
            self.input_summaries[input].clear();
            for &target in self.input_edges[input].iter()
            {
                if match target { ScopeInput(t, _) => self.subscopes[t].notify_me(), _ => true }
                {
                    self.input_summaries[input].push((target, Antichain::from_elem(self.default_summary)));
                }
            }
        }

        let mut done = false;
        while !done
        {
            done = true;

            // process edges from scope outputs ...
            for scope in range(0, self.subscopes.len())
            {
                for output in range(0, self.subscopes[scope].outputs())
                {
                    // for each target: ScopeOutput(scope, output) -> target ...
                    for target in self.scope_edges[scope][output].iter()
                    {
                        let next_sources = self.target_to_sources(target);
                        for &(next_source, next_summary) in next_sources.iter()
                        {
                            // this should always be true, because that is how t_2_s works.
                            if let ScopeOutput(next_scope, next_output) = next_source
                            {
                                // clone this so that we aren't holding a read ref to self.source_summaries.
                                let reachable = self.source_summaries[next_scope][next_output].clone();
                                for &(next_target, ref antichain) in reachable.iter()
                                {
                                    for summary in antichain.elements.iter()
                                    {
                                        let candidate_summary = next_summary.followed_by(summary);
                                        if try_to_add_summary(&mut self.source_summaries[scope][output], next_target, candidate_summary)
                                        {
                                            done = false;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // process edges from graph inputs ...
            for input in range(0, self.inputs())
            {
                // for each target: ScopeOutput(scope, output) -> target ...
                for target in self.input_edges[input].iter()
                {
                    let next_sources = self.target_to_sources(target);
                    for &(next_source, next_summary) in next_sources.iter()
                    {
                        // this should always be true, because that is how t_2_s works.
                        if let ScopeOutput(next_scope, next_output) = next_source
                        {
                            // clone this so that we aren't holding a read ref to self.source_summaries.
                            let reachable = self.source_summaries[next_scope][next_output].clone();
                            for &(next_target, ref antichain) in reachable.iter()
                            {
                                for summary in antichain.elements.iter()
                                {
                                    let candidate_summary = next_summary.followed_by(summary);
                                    if try_to_add_summary(&mut self.input_summaries[input], next_target, candidate_summary)
                                    {
                                        done = false;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // now that we are done, populate self.target_summaries
        for scope in range(0, self.subscopes.len())
        {
            for input in range(0, self.subscopes[scope].inputs())
            {
                self.target_summaries[scope][input].clear();

                let next_sources = self.target_to_sources(&ScopeInput(scope, input));

                for &(next_source, next_summary) in next_sources.iter()
                {
                    if let ScopeOutput(next_scope, next_output) = next_source
                    {
                        for &(next_target, ref antichain) in self.source_summaries[next_scope][next_output].iter()
                        {
                            for summary in antichain.elements.iter()
                            {
                                let candidate_summary = next_summary.followed_by(summary);
                                try_to_add_summary(&mut self.target_summaries[scope][input], next_target, candidate_summary);
                            }
                        }
                    }
                }
            }
        }
    }

    fn target_to_sources(&self, target: &Target) -> Vec<(Source, Summary<SOuter, SInner>)>
    {
        let mut result = Vec::new();

        match *target
        {
            GraphOutput(port) =>
            {
                for input in range(0, self.inputs())
                {
                    for &summary in self.external_summaries[port][input].elements.iter()
                    {
                        result.push((GraphInput(input), Outer(summary, Default::default())));
                    }
                }
            },
            ScopeInput(graph, port) =>
            {
                // this one is harder; propose connected output ports
                for i in range(0, self.subscopes[graph].outputs())
                {
                    for &summary in self.subscope_state[graph].summary[port][i].elements.iter()
                    {
                        result.push((ScopeOutput(graph, i), summary));
                    }
                }
            }
        }

        result
    }

    pub fn new_subgraph<T:Timestamp, S:PathSummary<T>>(&mut self, default: T, summary: S) ->
        Rc<RefCell<Subgraph<(TOuter, TInner), Summary<SOuter,SInner>, T, S>>>
    {
        let mut result: Subgraph<(TOuter, TInner), Summary<SOuter,SInner>, T, S> = Default::default();

        result.default_time = (Default::default(), default);
        result.default_summary = Local(summary);
        result.index = self.subscopes.len();

        return Rc::new(RefCell::new(result));
    }

    pub fn new_input(&mut self, shared_counts: Rc<RefCell<Vec<((TOuter, TInner), i64)>>>) -> uint
    {
        self.inputs += 1;

        self.external_guarantee.push(MutableAntichain::new());
        self.input_messages.push(shared_counts);

        return self.inputs - 1;
    }

    pub fn new_output(&mut self) -> uint
    {
        self.outputs += 1;
        self.external_capability.push(MutableAntichain::new());
        return self.outputs - 1;
    }

    pub fn connect(&mut self, source: Source, target: Target)
    {
        match source
        {
            ScopeOutput(scope, index) =>
            {
                while self.scope_edges.len() < scope + 1        { self.scope_edges.push(Vec::new()); }
                while self.scope_edges[scope].len() < index + 1 { self.scope_edges[scope].push(Vec::new()); }

                self.scope_edges[scope][index].push(target);
            },
            GraphInput(input) =>
            {
                while self.input_edges.len() < input + 1        { self.input_edges.push(Vec::new()); }

                self.input_edges[input].push(target);
            },
        }
    }
}

fn try_to_add_summary<S>(vector: &mut Vec<(Target, Antichain<S>)>, target: Target, summary: S) -> bool
where S: PartialOrd+Eq+Copy+Show
{
    for &(ref t, ref mut antichain) in vector.iter_mut()
    {
        if target.eq(t)
        {
            return antichain.insert(summary);
        }
    }

    vector.push((target, Antichain::from_elem(summary)));

    return true;
}
