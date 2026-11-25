using System.Threading.Tasks;

namespace App;

public class Downloader
{
    public async Task<string> FetchAsync(string url)
    {
        await Task.Yield();
        return url;
    }

    public async Task<int> SizeAsync(string url)
    {
        var body = await FetchAsync(url);
        return body.Length;
    }
}
