<?php
namespace Acme\Legacy {
    use Acme\Blog\{Entity\Post, Controller\BlogController as Blog};

    function make(): Post
    {
        return new Post('x');
    }
}

namespace {
    use Acme\Legacy;

    function legacy_boot(): void
    {
        Legacy\make()->publish();
        \Acme\Legacy\make();
    }
}
