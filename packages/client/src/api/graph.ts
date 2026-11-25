// The graph calls: nodes, edges, traversals and graph algorithms.

import {
  mapCypherResult,
  mapGraphEdges,
  mapGraphNode,
  mapImpact,
  mapNodeScores,
  mapWeightedPaths,
  toWireDirection,
  toWirePageRankConfig,
} from "../mappers.js";
import type {
  CallOptions,
  CentralityType,
  ComponentType,
  GraphAddEdgeOptions,
  GraphCentralityOptions,
  GraphDirection,
  GraphFindCyclesOptions,
  GraphListEdgesOptions,
  GraphListNodesOptions,
  GraphSetNodeOptions,
  GraphTraversalOptions,
  JsonValue,
  PageRankConfig,
} from "../types.js";
import { type Calls, type Mode } from "./calls.js";

export function graphApi<M extends Mode>({ call, write, weightedPath, flag }: Calls<M>) {
  return {
    setNode: (
      graph: string,
      id: string,
      kind: string,
      metadata?: JsonValue,
      options: GraphSetNodeOptions = {},
    ) =>
      write(
        {
          model: "graph_set_node",
          graph,
          id,
          kind,
          metadata,
          docSpace: options.docSpace,
          docId: options.docId,
        },
        options.signal,
      ),
    getNode: (graph: string, id: string, options: CallOptions = {}) =>
      call(
        { type: "graph_get_node", graph, id },
        "graph_node",
        (node) => (node ? mapGraphNode(node) : undefined),
        options.signal,
      ),
    getNodeDocument: <T = JsonValue>(graph: string, id: string, options: CallOptions = {}) =>
      call(
        { type: "graph_get_node_document", graph, id },
        "json_extracted",
        (value) => (value === null ? undefined : (value as T)),
        options.signal,
      ),
    deleteNode: (graph: string, id: string, options: CallOptions = {}) =>
      flag({ type: "graph_delete_node", graph, id }, options.signal),
    listNodes: (graph: string, options: GraphListNodesOptions = {}) =>
      call(
        { type: "graph_list_nodes", graph, kind: options.kind, limit: options.limit ?? 0 },
        "graph_nodes",
        (nodes) => nodes.map(mapGraphNode),
        options.signal,
      ),
    addEdge: (
      graph: string,
      source: string,
      target: string,
      kind: string,
      options: GraphAddEdgeOptions = {},
    ) =>
      write(
        { model: "graph_add_edge", graph, source, target, kind, weight: options.weight },
        options.signal,
      ),
    removeEdge: (
      graph: string,
      source: string,
      target: string,
      kind: string,
      options: CallOptions = {},
    ) => flag({ type: "graph_remove_edge", graph, source, target, kind }, options.signal),
    listEdges: (graph: string, options: GraphListEdgesOptions = {}) =>
      call(
        {
          type: "graph_list_edges",
          graph,
          source: options.source,
          target: options.target,
          kind: options.kind,
          limit: options.limit ?? 0,
        },
        "graph_edges",
        mapGraphEdges,
        options.signal,
      ),
    neighbors: (
      graph: string,
      node: string,
      options: { direction?: GraphDirection; kind?: string } & CallOptions = {},
    ) =>
      call(
        {
          type: "graph_neighbors",
          graph,
          node,
          direction: toWireDirection(options.direction),
          kind: options.kind,
        },
        "ids",
        (ids) => ids,
        options.signal,
      ),
    reachability: (
      graph: string,
      startNodes: string[],
      options: { direction?: GraphDirection } & CallOptions = {},
    ) =>
      call(
        {
          type: "graph_reachability",
          graph,
          start_nodes: startNodes,
          direction: toWireDirection(options.direction),
        },
        "ids",
        (ids) => ids,
        options.signal,
      ),
    impactAnalysis: (graph: string, changedNodes: string[], options: CallOptions = {}) =>
      call(
        { type: "graph_impact_analysis", graph, changed_nodes: changedNodes },
        "graph_impact",
        mapImpact,
        options.signal,
      ),
    shortestPath: (
      graph: string,
      start: string,
      target: string,
      options: GraphTraversalOptions = {},
    ) =>
      weightedPath(
        {
          type: "graph_shortest_path",
          graph,
          start,
          target,
          direction: toWireDirection(options.direction),
          kind_filter: options.kindFilter,
        },
        options.signal,
      ),
    dijkstra: (
      graph: string,
      start: string,
      target?: string,
      options: GraphTraversalOptions = {},
    ) =>
      weightedPath(
        {
          type: "graph_dijkstra",
          graph,
          start,
          target,
          direction: toWireDirection(options.direction),
          kind_filter: options.kindFilter,
        },
        options.signal,
      ),
    allShortestPaths: (graph: string, start: string, options: GraphTraversalOptions = {}) =>
      call(
        {
          type: "graph_all_shortest_paths",
          graph,
          start,
          direction: toWireDirection(options.direction),
          kind_filter: options.kindFilter,
        },
        "weighted_path_map",
        mapWeightedPaths,
        options.signal,
      ),
    astarSpatial: (
      graph: string,
      start: string,
      target: string,
      options: GraphTraversalOptions = {},
    ) =>
      weightedPath(
        {
          type: "graph_astar_spatial",
          graph,
          start,
          target,
          direction: toWireDirection(options.direction),
          kind_filter: options.kindFilter,
        },
        options.signal,
      ),
    pagerank: (graph: string, config: PageRankConfig & CallOptions = {}) => {
      const { signal, ...rest } = config;
      return call(
        { type: "graph_page_rank", graph, config: toWirePageRankConfig(rest) },
        "node_scores",
        mapNodeScores,
        signal,
      );
    },
    centrality: (graph: string, centrality: CentralityType, options: GraphCentralityOptions = {}) =>
      call(
        {
          type: "graph_centrality",
          graph,
          centrality,
          direction: toWireDirection(options.direction),
          kind_filter: options.kindFilter,
          normalized: options.normalized ?? false,
        },
        "node_scores",
        mapNodeScores,
        options.signal,
      ),
    hasCycle: (graph: string, options: GraphTraversalOptions = {}) =>
      flag(
        {
          type: "graph_has_cycle",
          graph,
          direction: toWireDirection(options.direction),
          kind_filter: options.kindFilter,
        },
        options.signal,
      ),
    findCycles: (graph: string, options: GraphFindCyclesOptions = {}) =>
      call(
        {
          type: "graph_find_cycles",
          graph,
          max_cycles: options.maxCycles ?? 100,
          kind_filter: options.kindFilter,
        },
        "node_groups",
        (cycles) => cycles,
        options.signal,
      ),
    connectedComponents: (
      graph: string,
      componentType: ComponentType,
      options: { kindFilter?: string } & CallOptions = {},
    ) =>
      call(
        {
          type: "graph_connected_components",
          graph,
          component_type: componentType,
          kind_filter: options.kindFilter,
        },
        "node_groups",
        (components) => components,
        options.signal,
      ),
    cypher: <T = Record<string, JsonValue>>(
      graph: string,
      query: string,
      options: CallOptions = {},
    ) =>
      call(
        { type: "graph_cypher", graph, query },
        "cypher_result",
        (result) => mapCypherResult<T>(result),
        options.signal,
      ),
  };
}
