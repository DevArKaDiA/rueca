<?php
class Report
{
    public function render(): string
    {
        return $this->header() . $this->body();
    }

    private function header(): string
    {
        return 'h';
    }

    private function body(): string
    {
        return 'b';
    }
}
