namespace App;

public class Base
{
    protected static int Helper()
    {
        return 1;
    }
}

public class Derived : Base
{
    public int Use()
    {
        return Helper();
    }
}
