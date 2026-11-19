/// <reference types="react" />

declare module "node:assert/strict" {
  const assert: {
    equal<T>(actual: unknown, expected: T, message?: string): asserts actual is T;
    notEqual(actual: unknown, expected: unknown, message?: string): void;
    ok(value: unknown, message?: string): asserts value;
    match(value: string, regexp: RegExp, message?: string): void;
    throws(
      fn: () => unknown,
      error?: RegExp | ((err: unknown) => boolean),
      message?: string,
    ): void;
  };
  export default assert;
}

declare module "node:test" {
  interface TestContext {
    readonly name: string;
  }
  type TestFn = (t: TestContext) => void | Promise<void>;
  export default function test(name: string, fn: TestFn): void;
}

declare module "node:net" {
  export interface AddressInfo {
    address: string;
    family: string;
    port: number;
  }
}
