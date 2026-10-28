declare function amb(x: number): void;
declare function amb(x: string): void;
export function f() { amb(1); amb('s'); }
