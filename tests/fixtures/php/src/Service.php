<?php

declare(strict_types=1);

namespace Fixture;

use Fixture\Models\Base;
use Fixture\Models\Engine;
use Fixture\Models\Region;
use GuzzleHttp\Client;

function build(Region $region): Engine
{
    $engine = new Engine($region);
    $engine->ping();

    return $engine;
}

function inspect(Base $item): string
{
    return $item->describe();
}

function fetchOne(int $ident): string
{
    $client = new Client();
    $response = $client->get("/engines/{$ident}");

    return (string) $response->getBody();
}

function defaultRegion(): Region
{
    return Region::from('eu');
}
