<?php
class Job
{
    public function run(): void
    {
        $this->step();
    }

    protected function step(): void
    {
    }
}

class SpecialJob extends Job
{
    protected function step(): void
    {
        $this->extra();
    }

    private function extra(): void
    {
    }
}
