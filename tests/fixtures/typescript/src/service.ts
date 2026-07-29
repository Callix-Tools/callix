/** Calls, references, annotations and a boundary. */

import { Base, Engine, type Identifier } from "@/store";

export const DEFAULT_REGION: string = "eu";

export function build(region: string): Engine {
  const engine = new Engine(region);
  engine.ping();
  return engine;
}

export function inspect(item: Base): string {
  return item.describe();
}

export async function fetchOne(ident: Identifier): Promise<string> {
  const response = await fetch(`/engines/${ident}`);
  return response.text();
}
