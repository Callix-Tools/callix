/** Types the service annotates against. */

export type Identifier = number;

export interface Labeled {
  describe(): string;
}

export class Base implements Labeled {
  label: string = "base";

  describe(): string {
    return this.label;
  }
}

export class Engine extends Base {
  region: string;

  constructor(region: string) {
    super();
    this.region = region;
  }

  ping(): boolean {
    return this.region.length > 0;
  }
}
