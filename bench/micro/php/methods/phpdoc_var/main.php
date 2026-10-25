<?php
class Cache
{
    public function get(string $k): mixed
    {
        return null;
    }
}

class Container
{
    public function service(string $id): object
    {
        return new Cache();
    }
}

function lookup(Container $c): mixed
{
    /** @var Cache $cache */
    $cache = $c->service('cache');
    return $cache->get('k');
}
