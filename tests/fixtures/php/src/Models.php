<?php

declare(strict_types=1);

namespace Fixture\Models;

/** Types the service layer annotates against. */
interface Labeled
{
    public function describe(): string;
}

trait Describes
{
    public function describe(): string
    {
        return $this->label;
    }
}

enum Region: string
{
    case Europe = 'eu';
    case America = 'us';
}

class Base implements Labeled
{
    use Describes;

    public const DEFAULT_LABEL = 'base';

    protected string $label = self::DEFAULT_LABEL;
}

final class Engine extends Base
{
    public function __construct(private Region $region)
    {
    }

    public function ping(): bool
    {
        return true;
    }

    public function region(): Region
    {
        return $this->region;
    }
}
