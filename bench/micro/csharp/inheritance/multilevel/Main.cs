namespace App;

public class A
{
    public void Root()
    {
    }
}

public class B : A
{
}

public class C : B
{
    public void Leaf()
    {
        Root();
    }
}
