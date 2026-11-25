using System;

namespace App;

public class Events
{
    public void Fire()
    {
    }

    public Action Bind()
    {
        return () => Fire();
    }
}
