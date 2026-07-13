# Graphify Local Index

Graphify output is local generated state and should live under `.local/graphify/`.

The current merged code graph is:

```text
.local/graphify/root/graph.json
```

Build the backend code graph:

```sh
/home/dev/.local/bin/graphify extract src --code-only --no-cluster --max-workers 2 --out .local/graphify/build
rm -rf .local/graphify/root
mv .local/graphify/build/graphify-out .local/graphify/root
```

Build the frontend code graph:

```sh
/home/dev/.local/bin/graphify update web/ts --no-cluster
mv web/ts/graphify-out .local/graphify/web-ts/out
```

Merge the two graphs:

```sh
/home/dev/.local/bin/graphify merge-graphs \
  .local/graphify/root/graph.json \
  .local/graphify/web-ts/out/graph.json \
  --out .local/graphify/root/restream-code-graph.json
cp .local/graphify/root/restream-code-graph.json .local/graphify/root/graph.json
```

Query the merged graph:

```sh
/home/dev/.local/bin/graphify explain "StageGraphPlan" --graph .local/graphify/root/graph.json
/home/dev/.local/bin/graphify path "StageGraphPlan" "pipeline_graph_handler" --graph .local/graphify/root/graph.json
```
