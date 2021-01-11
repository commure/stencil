# Stencil: collect flame graphs and flame charts

When enabled, the traces are logged, via the `log` crate, at `warn!` level. If you want to
handle them differently, you can: see the `set_flame_graph_handler` and
`set_flame_chart_handler` functions.

Once a trace is logged, it is up to you to convert it to an svg. We recommend using
`flamegraph.pl` from https://github.com/brendangregg/FlameGraph, as:

> flamegraph.pl trace.txt > trace.svg

## What is the difference betweeen a flame graph and a flame chart?

A flame chart shows the precise timing of each call. If function A calls function B 100 times,
it will show up in the picture as 100 separate calls. This is both a blessing (it shows the
order of events, and has a very clear interpretation), and a curse (it scales poorly for
measuring frequent but brief calls).

In a flame graph, on the other hand, if A calls B 100 times, all the calls to B will be lumped
together into one span showing their total time.

There is a good overview of flame graphs in this readme for a different project:
https://github.com/flamegraph-rs/flamegraph#systems-performance-work-guided-by-flamegraphs

## How do I use this crate?

To declare a span:
```
use stencil::*;
fn myExistingFunctionRelatedToKittens() {
  outln!("kittens"); // measures the span from now until it's dropped
  // kitten related functionality
}
```

To log flame graphs:
```
use stencil::*;
enable_flame_graph_collection(true);
set_flame_graph_handler(Box::new(|trace| {
  let flamegraph: String = trace.as_flame_graph_input().unwrap();
  Some(format!("flamegraph: {}", flamegraph)) // will be logged
}));
```

To log flame charts:
```
use stencil::*;
enable_flame_chart_collection(true);
set_flame_chart_handler(Box::new(|callsites| {
  let flamechart: String = call_sites_to_flame_chart(callsites);
  Some(format!("flamechart: {}", flamechart)) // will be logged
}));
```

When collecting flame charts, there is a maximum number of events per trace, so calling `outln!` on
a function likely to be called inside a hot loop is not recommended. In contrast, flame graphs are
happy to handle hot loops, since they will group all the calls together.

If you want to do something different with the traces instead of logging them, return `None`
from the callback you pass to `set_flame_XXX_handler`.
