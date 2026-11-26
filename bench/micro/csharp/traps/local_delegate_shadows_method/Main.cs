using System;

namespace App;

public class Runner
{
    public void Step()
    {
    }

    public void Go()
    {
        Action Step = () => { };
        Step();
    }
}
