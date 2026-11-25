namespace App;

public class Node
{
    public void Visit()
    {
    }
}

public class Walker
{
    public void Walk(object a, object b)
    {
        ((Node)a).Visit();
        (b as Node).Visit();
    }
}
