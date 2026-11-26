namespace App;

public class Container
{
    public static class Keys
    {
        public static string Make()
        {
            return "k";
        }
    }
}

public class Client
{
    public string Key()
    {
        return Container.Keys.Make();
    }
}
