<?php
class Plain
{
    public function go(): void
    {
    }
}

function run(): void
{
    (new Plain())->go();
}
