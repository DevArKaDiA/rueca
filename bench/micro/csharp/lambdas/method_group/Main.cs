using System;

namespace App;

public class Scheduler
{
    public void Tick()
    {
    }

    public Action Callback()
    {
        return Tick;
    }
}
