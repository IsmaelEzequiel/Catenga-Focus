import type { Range } from "./time";

export interface Lane {
  lane: number;
  lanes: number;
}

/** Side-by-side columns for overlapping blocks; result is index-aligned with the input. */
export function layoutLanes(blocks: Range[]): Lane[] {
  const result: Lane[] = blocks.map(() => ({ lane: 0, lanes: 1 }));
  const order = blocks.map((_, i) => i).sort((a, b) => blocks[a].start - blocks[b].start || blocks[a].end - blocks[b].end);

  let cluster: number[] = [];
  let clusterEnd = -1;
  let laneEnds: number[] = [];

  const flush = () => {
    for (const i of cluster) result[i].lanes = laneEnds.length;
    cluster = [];
    laneEnds = [];
  };

  for (const i of order) {
    const { start, end } = blocks[i];
    if (cluster.length > 0 && start >= clusterEnd) flush();
    let lane = laneEnds.findIndex((e) => e <= start);
    if (lane === -1) {
      lane = laneEnds.length;
      laneEnds.push(end);
    } else {
      laneEnds[lane] = end;
    }
    result[i].lane = lane;
    cluster.push(i);
    clusterEnd = Math.max(clusterEnd, end);
  }
  flush();
  return result;
}
