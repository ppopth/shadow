#!/usr/bin/env python3

# Used to generate a connected graph used in the gossipsub network

import networkx as nx
import random
import sys

if len(sys.argv) < 3:
    print("Usage: ./gen_graph.py [number-of-nodes] [number-of-edges]")
    exit(1)

num_nodes = int(sys.argv[1])
num_edges = int(sys.argv[2])

if num_edges < num_nodes - 1:
    print("The number of edges must be at least the number of nodes minus one")
    exit(1)

# We generate a uniformly random connected graph with a given number of nodes and edges by
# 1. Generate a uniformly random spanning tree across all nodes
# 2. Randomly put more edges into the tree until the expected number of edges is reached
# See more at https://stackoverflow.com/a/14618505

# First, implement Wilson's algorithm to generate a uniform spanning tree
S = set(range(num_nodes))
T = set()
graph = nx.empty_graph(num_nodes)

v = random.choices(list(S), k=1)[0]
S.remove(v)
T.add(v)

while len(T) < num_nodes:
    path = list()
    v = random.choices(list(S), k=1)[0]
    path.append(v)
    while not v in T:
        v = random.randrange(num_nodes)
        while v in path:
            path.pop()
        path.append(v)

    # Add the path to the graph
    p = path[0]
    for v in path[1:]:
        graph.add_edge(p, v)
        p = v

    # Add the path to the tree
    path.pop()
    for v in path:
        S.remove(v)
        T.add(v)

# Make sure that it's a tree
assert nx.is_tree(graph)

# Second, add more random edges to the tree until the expected number of edges is reached
while graph.size() != num_edges:
    u = random.randrange(num_nodes)
    v = random.randrange(num_nodes)
    if u != v:
        graph.add_edge(u, v)

for line in nx.generate_adjlist(graph):
    print(line)
