using System.Net;
using System.Net.Http.Headers;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;
using StockIpoReminder.Core.Abstractions;
using StockIpoReminder.Core.Services;
using StockIpoReminder.Infrastructure.Announcements;
using StockIpoReminder.Infrastructure.Collectors;
using StockIpoReminder.Infrastructure.Operations;
using StockIpoReminder.Infrastructure.Persistence;
using StockIpoReminder.Infrastructure.Runtime;

namespace StockIpoReminder.Infrastructure;

public static class DependencyInjection
{
    private static readonly string ProductUserAgent = $"StockIpoReminder/{typeof(DependencyInjection).Assembly.GetName().Version?.ToString(3) ?? "0.1.1"}";

    public static IServiceCollection AddStockIpoReminderInfrastructure(
        this IServiceCollection services,
        string dataRoot,
        bool enableHostedServices = true)
    {
        Directory.CreateDirectory(dataRoot);
        var maintenanceOptions = new MaintenanceOptions
        {
            DataRoot = dataRoot,
            LogDirectory = Path.Combine(dataRoot, "logs"),
            BackupDirectory = Path.Combine(dataRoot, "backups"),
            DiagnosticDirectory = Path.Combine(dataRoot, "diagnostics"),
        };
        services.AddSingleton(TimeProvider.System);
        services.AddSingleton(new DatabaseOptions { DatabasePath = Path.Combine(dataRoot, "stock-ipo-reminder.db") });
        services.AddSingleton(new AnnouncementOptions { StorageDirectory = Path.Combine(dataRoot, "announcements") });
        services.AddSingleton(maintenanceOptions);
        services.AddSingleton<IIpoRepository, SqliteIpoRepository>();
        services.AddSingleton<PersistenceInspectionService>();
        services.AddSingleton<IpoReconciler>();
        services.AddSingleton<ReminderPlanner>();
        services.AddSingleton<AnnouncementFieldParser>();
        services.AddSingleton<RuntimeState>();
        services.AddSingleton(new SourceBackoffOptions());
        services.AddSingleton<SourceBackoffStore>();

        services.AddHttpClient<EastmoneyCollector>(ConfigureJsonClient)
            .ConfigurePrimaryHttpMessageHandler(CreateJsonHttpHandler);
        services.AddHttpClient<SseCollector>(client =>
        {
            ConfigureJsonClient(client);
            client.DefaultRequestHeaders.Referrer = new Uri("https://www.sse.com.cn/");
        }).ConfigurePrimaryHttpMessageHandler(CreateJsonHttpHandler);
        services.AddHttpClient<CninfoCollector>(client =>
        {
            ConfigureJsonClient(client);
            client.DefaultRequestHeaders.Referrer = new Uri("https://www.cninfo.com.cn/new/index");
        }).ConfigurePrimaryHttpMessageHandler(CreateJsonHttpHandler);
        services.AddHttpClient<BseCollector>(client =>
        {
            ConfigureJsonClient(client);
            client.DefaultRequestHeaders.Referrer = new Uri("https://www.bseinfo.net/newshare/listofissues.html");
        }).ConfigurePrimaryHttpMessageHandler(CreateBseHttpHandler);

        services.AddTransient<IIpoCollector>(provider => provider.GetRequiredService<EastmoneyCollector>());
        services.AddTransient<IIpoCollector>(provider => provider.GetRequiredService<SseCollector>());
        services.AddTransient<IIpoCollector>(provider => provider.GetRequiredService<CninfoCollector>());
        services.AddTransient<IIpoCollector>(provider => provider.GetRequiredService<BseCollector>());

        services.AddHttpClient<SseAnnouncementProvider>(client =>
        {
            ConfigureJsonClient(client);
            client.DefaultRequestHeaders.Referrer = new Uri("https://www.sse.com.cn/");
        }).ConfigurePrimaryHttpMessageHandler(CreateJsonHttpHandler);
        services.AddHttpClient<CninfoAnnouncementProvider>(client =>
        {
            ConfigureJsonClient(client);
            client.DefaultRequestHeaders.Referrer = new Uri("https://www.cninfo.com.cn/new/index");
        }).ConfigurePrimaryHttpMessageHandler(CreateCookieHttpHandler);
        services.AddHttpClient<BseAnnouncementProvider>(client =>
        {
            ConfigureJsonClient(client);
            client.DefaultRequestHeaders.Referrer = new Uri("https://www.bseinfo.net/newshare/listofissues.html");
        }).ConfigurePrimaryHttpMessageHandler(CreateBseHttpHandler);
        services.AddTransient<IAnnouncementProvider>(provider => provider.GetRequiredService<SseAnnouncementProvider>());
        services.AddTransient<IAnnouncementProvider>(provider => provider.GetRequiredService<CninfoAnnouncementProvider>());
        services.AddTransient<IAnnouncementProvider>(provider => provider.GetRequiredService<BseAnnouncementProvider>());
        services.AddHttpClient<AnnouncementProcessor>(ConfigureDownloadClient)
            .ConfigurePrimaryHttpMessageHandler(CreateCookieHttpHandler);
        services.AddTransient<IAnnouncementProcessor>(provider => provider.GetRequiredService<AnnouncementProcessor>());
        services.AddHttpClient("system-clock", client =>
        {
            client.Timeout = TimeSpan.FromSeconds(10);
            client.DefaultRequestHeaders.UserAgent.ParseAdd($"{ProductUserAgent} clock-check");
        });

        services.AddSingleton<SynchronizationService>();
        services.AddSingleton<SyncCoordinatorHostedService>();
        services.AddSingleton<ISyncTrigger>(provider => provider.GetRequiredService<SyncCoordinatorHostedService>());
        services.AddSingleton<DailyHealthSummaryCoordinator>();
        services.AddSingleton<ReminderDeliveryHostedService>();
        services.AddSingleton<ReminderLifecycleService>();
        services.AddSingleton<ReminderManagementService>();
        services.AddSingleton<DiagnosticBundleService>();
        services.AddSingleton<OperationalMaintenanceService>();
        services.AddSingleton(new RecoveryEventOptions());
        services.AddSingleton<RecoveryEventCoordinator>();
        services.AddSingleton(new SystemClockOptions());
        services.AddSingleton(provider => new SystemClockMonitor(
            provider.GetRequiredService<IHttpClientFactory>().CreateClient("system-clock"),
            provider.GetRequiredService<SystemClockOptions>(),
            provider.GetRequiredService<RuntimeState>(),
            provider.GetRequiredService<TimeProvider>(),
            provider.GetRequiredService<Microsoft.Extensions.Logging.ILogger<SystemClockMonitor>>()));
        services.AddSingleton<ISystemClockCheckTrigger>(provider => provider.GetRequiredService<SystemClockMonitor>());
        if (enableHostedServices)
        {
            services.AddSingleton<IHostedService>(provider => provider.GetRequiredService<SyncCoordinatorHostedService>());
            services.AddSingleton<IHostedService>(provider => provider.GetRequiredService<ReminderDeliveryHostedService>());
            services.AddSingleton<IHostedService>(provider => provider.GetRequiredService<OperationalMaintenanceService>());
            services.AddSingleton<IHostedService>(provider => provider.GetRequiredService<SystemClockMonitor>());
        }

        return services;
    }

    private static void ConfigureJsonClient(HttpClient client)
    {
        client.Timeout = TimeSpan.FromSeconds(25);
        client.DefaultRequestHeaders.UserAgent.ParseAdd($"Mozilla/5.0 (Windows NT 10.0; Win64; x64) {ProductUserAgent}");
        client.DefaultRequestHeaders.Accept.Add(new MediaTypeWithQualityHeaderValue("application/json"));
        client.DefaultRequestHeaders.AcceptEncoding.ParseAdd("gzip, deflate, br");
    }

    private static void ConfigureDownloadClient(HttpClient client)
    {
        client.Timeout = TimeSpan.FromSeconds(45);
        client.DefaultRequestHeaders.UserAgent.ParseAdd($"Mozilla/5.0 (Windows NT 10.0; Win64; x64) {ProductUserAgent}");
        client.DefaultRequestHeaders.Accept.ParseAdd("application/pdf,text/html;q=0.9,*/*;q=0.8");
    }

    private static HttpMessageHandler CreateJsonHttpHandler() => new HttpClientHandler
    {
        AutomaticDecompression = DecompressionMethods.All,
    };

    private static HttpMessageHandler CreateBseHttpHandler() => new HttpClientHandler
    {
        AutomaticDecompression = DecompressionMethods.All,
        UseCookies = true,
        CookieContainer = new CookieContainer(),
    };

    private static HttpMessageHandler CreateCookieHttpHandler() => new HttpClientHandler
    {
        AutomaticDecompression = DecompressionMethods.All,
        UseCookies = true,
        CookieContainer = new CookieContainer(),
    };
}
