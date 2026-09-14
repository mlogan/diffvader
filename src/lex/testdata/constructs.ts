#!/usr/bin/env node
// Lexer fixture: constructs where lexer-level context decides the highlight. The oracle
// test checks this file against tree-sitter. It is never compiled.

import * as path from "node:path";
import fs, { readFileSync as read, type Stats } from "node:fs";
export { read as readFile };
export type { Stats };

export const MAX_DEPTH = 10;
const PATTERN = /^[a-z]+\/(\d+)$/gi;
const ratio = total / count / 2;
declare function signature(a: string): void;

type Mapped<T> = { readonly [K in keyof T]?: T[K] extends Function ? never : T[K] };
type Tuple = [first: string, rest?: number];
type Pred = (value: unknown) => value is string;
type Template<N extends string = "x"> = `prefix-${N}-suffix`;
type Ctor = abstract new (...args: any[]) => object;
type Sym = { readonly id: unique symbol };

interface Shape<T = number> extends Base<T> {
  readonly kind: "circle" | "square";
  area(): T;
  new (size: T): Shape<T>;
  [key: string]: unknown;
}

enum Color {
  Red = 1,
  Green = Red << 1,
}

abstract class Widget<P extends object> extends Base<P> implements Drawable {
  static count = 0;
  #secret = 42;
  private readonly name: string;
  protected handler = (e: Event): void => this.onEvent(e);
  declare label?: string;

  constructor(private readonly options: P, public id = Widget.count++) {
    super(options);
  }

  get size(): number {
    return this.#secret;
  }

  abstract render(): string;

  async *items<T>(this: Widget<P>, source: Iterable<T>): AsyncGenerator<T> {
    for (const item of source) {
      yield item;
    }
  }

  isReady(): this is ReadyWidget {
    return !!this.options;
  }
}

function assertDefined<T>(value: T | undefined, message?: string): asserts value is T {
  if (value === undefined) throw new Error(`missing: ${message ?? "value"}`);
}

export default async function main(args: string[]): Promise<number> {
  const { verbose = false, ...rest } = parse(args);
  const map = new Map<string, Array<{ id: number }>>();
  const callback = async (x: number) => x * 2;
  const label = verbose ? `${rest.length} args` : "quiet";
  let result = await compute<number>(ratio);
  result ??= 0;
  const obj = {
    name: "widget",
    run() {
      return result;
    },
    handle: (event: string) => console.log(event),
    [Symbol.iterator]: function* () {},
    MAX_DEPTH,
  };
  try {
    fs.statSync(path.join(__dirname, label))?.isFile?.();
  } catch {
    return 1;
  }
  switch (result) {
    case 0:
      break;
    default: {
      obj.run();
    }
  }
  return (obj satisfies object) as unknown as number;
}
