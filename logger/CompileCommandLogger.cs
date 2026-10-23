using System;
using System.Collections;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Reflection;
using System.Text;
using Microsoft.Build.Framework;

namespace LazyMsBuild.Logger
{
    /// <summary>
    /// Records the build events lazymsbuild needs, one JSON object per line (JSON Lines).
    /// It does no parsing: lazymsbuild tokenizes the command lines and splits batched compiles.
    /// </summary>
    /// <remarks>
    /// Usage: <c>-logger:LazyMsBuild.Logger.CompileCommandLogger,path\to\LazyMsBuild.Logger.dll;path\to\events.jsonl</c>.
    /// Everything after the first ';' is the output path. Do not put '"' inside the switch: MSBuild
    /// splits its own command line and does not understand \" escapes; quote the whole argument instead.
    ///
    /// Each line is flushed as it is written, so a build whose process tree is killed still leaves
    /// every complete line behind. A logger that throws stops the whole build (MSB4017), so every
    /// handler swallows its own failures and a write failure only stops the logging.
    ///
    /// Lines (format version 1):
    ///   {"t":"hello","v":1,"msbuild":"17.14.51.32402"}
    ///   {"t":"eval","node":1,"eval":3,"project":"...","props":{"IncludePath":"...","ExternalIncludePath":"..."}}
    ///   {"t":"pstart","node":1,"ctx":7,"eval":3,"parent":4,"project":"...","targets":"Rebuild","cfg":"Debug|x64","props":{...}}
    ///   {"t":"cmd","node":1,"ctx":7,"project":"...","task":"CL","cmd":"C:\...\CL.exe /c ... a.cpp b.cpp"}
    ///   {"t":"diag","sev":"error","node":1,"ctx":7,"project":"...","file":"...","line":3,"col":5,"code":"C2065","msg":"..."}
    ///   {"t":"pend","node":1,"ctx":7,"project":"...","ok":true}
    ///   {"t":"end","ok":true}
    /// "props" carries the properties that MSBuild's VC targets turn into cl.exe's INCLUDE and
    /// EXTERNAL_INCLUDE environment variables; they never appear on the compiler command line.
    /// They come from the evaluation (MSBuild 16.10+) or, on older MSBuild, from ProjectStarted.
    /// </remarks>
    public sealed class CompileCommandLogger : ILogger
    {
        /// <summary>Version of the line format. lazymsbuild refuses a file with a version it does not know.</summary>
        public const int FormatVersion = 1;

        private static readonly string[] CapturedProperties = { "IncludePath", "ExternalIncludePath" };

        private readonly object gate = new object();
        private readonly Dictionary<Type, PropertyAccessors> accessors = new Dictionary<Type, PropertyAccessors>();
        private StreamWriter writer;

        public LoggerVerbosity Verbosity { get; set; }

        public string Parameters { get; set; }

        public void Initialize(IEventSource eventSource)
        {
            string path = (Parameters ?? string.Empty).Trim().Trim('"');
            if (path.Length == 0)
            {
                path = "lazymsbuild-events.jsonl";
            }

            try
            {
                var stream = new FileStream(path, FileMode.Create, FileAccess.Write, FileShare.Read);
                writer = new StreamWriter(stream, new UTF8Encoding(false)) { AutoFlush = true, NewLine = "\n" };
            }
            catch (Exception)
            {
                // lazymsbuild notices the missing file and reports it; the build itself goes on.
                writer = null;
                return;
            }

            Write(new StringBuilder("{\"t\":\"hello\",\"v\":").Append(FormatVersion)
                .Append(",\"msbuild\":").Append(Json(MsBuildVersion()))
                .Append('}'));

            // Ask for the evaluated properties on the evaluation event (MSBuild 16.10+). When every
            // logger asks, MSBuild stops copying all properties onto each ProjectStarted event.
            var withEvaluation = eventSource as IEventSource4;
            if (withEvaluation != null)
            {
                withEvaluation.IncludeEvaluationPropertiesAndItems();
            }

            eventSource.StatusEventRaised += OnStatusEventRaised;
            eventSource.ProjectStarted += OnProjectStarted;
            eventSource.ProjectFinished += OnProjectFinished;
            eventSource.MessageRaised += OnMessageRaised;
            eventSource.ErrorRaised += OnErrorRaised;
            eventSource.WarningRaised += OnWarningRaised;
            eventSource.BuildFinished += OnBuildFinished;
        }

        public void Shutdown()
        {
            lock (gate)
            {
                if (writer != null)
                {
                    try { writer.Dispose(); } catch (Exception) { }
                    writer = null;
                }
            }
        }

        private void OnStatusEventRaised(object sender, BuildStatusEventArgs e)
        {
            try
            {
                var evaluation = e as ProjectEvaluationFinishedEventArgs;
                if (evaluation == null)
                {
                    return;
                }

                var line = new StringBuilder("{\"t\":\"eval\"");
                AppendNode(line, evaluation.BuildEventContext);
                if (evaluation.BuildEventContext != null)
                {
                    line.Append(",\"eval\":").Append(evaluation.BuildEventContext.EvaluationId);
                }
                line.Append(",\"project\":").Append(Json(evaluation.ProjectFile));
                AppendProperties(line, evaluation.Properties);
                Write(line.Append('}'));
            }
            catch (Exception)
            {
            }
        }

        private void OnProjectStarted(object sender, ProjectStartedEventArgs e)
        {
            try
            {
                var line = new StringBuilder("{\"t\":\"pstart\"");
                AppendNode(line, e.BuildEventContext);
                AppendContext(line, e.BuildEventContext);
                if (e.BuildEventContext != null)
                {
                    line.Append(",\"eval\":").Append(e.BuildEventContext.EvaluationId);
                }
                if (e.ParentProjectBuildEventContext != null && e.ParentProjectBuildEventContext.ProjectContextId >= 0)
                {
                    line.Append(",\"parent\":").Append(e.ParentProjectBuildEventContext.ProjectContextId);
                }
                line.Append(",\"project\":").Append(Json(e.ProjectFile))
                    .Append(",\"targets\":").Append(Json(e.TargetNames));
                string configuration = GlobalProperty(e.GlobalProperties, "Configuration");
                string platform = GlobalProperty(e.GlobalProperties, "Platform");
                if (configuration != null || platform != null)
                {
                    line.Append(",\"cfg\":").Append(Json((configuration ?? string.Empty) + "|" + (platform ?? string.Empty)));
                }
                AppendProperties(line, e.Properties);
                Write(line.Append('}'));
            }
            catch (Exception)
            {
            }
        }

        private void OnProjectFinished(object sender, ProjectFinishedEventArgs e)
        {
            try
            {
                var line = new StringBuilder("{\"t\":\"pend\"");
                AppendNode(line, e.BuildEventContext);
                AppendContext(line, e.BuildEventContext);
                line.Append(",\"project\":").Append(Json(e.ProjectFile))
                    .Append(",\"ok\":").Append(e.Succeeded ? "true" : "false");
                Write(line.Append('}'));
            }
            catch (Exception)
            {
            }
        }

        private void OnMessageRaised(object sender, BuildMessageEventArgs e)
        {
            try
            {
                // Every tool command line, not only the CL task's: with UseMultiToolTask some compiles
                // are logged by TrackedExec. lazymsbuild picks the compiler by the executable name.
                var command = e as TaskCommandLineEventArgs;
                if (command == null)
                {
                    return;
                }

                var line = new StringBuilder("{\"t\":\"cmd\"");
                AppendNode(line, command.BuildEventContext);
                AppendContext(line, command.BuildEventContext);
                line.Append(",\"project\":").Append(Json(command.ProjectFile))
                    .Append(",\"task\":").Append(Json(command.TaskName))
                    .Append(",\"cmd\":").Append(Json(command.CommandLine));
                Write(line.Append('}'));
            }
            catch (Exception)
            {
            }
        }

        private void OnErrorRaised(object sender, BuildErrorEventArgs e)
        {
            try
            {
                WriteDiagnostic("error", e.BuildEventContext, e.ProjectFile, e.File, e.LineNumber, e.ColumnNumber, e.Code, e.Message);
            }
            catch (Exception)
            {
            }
        }

        private void OnWarningRaised(object sender, BuildWarningEventArgs e)
        {
            try
            {
                WriteDiagnostic("warning", e.BuildEventContext, e.ProjectFile, e.File, e.LineNumber, e.ColumnNumber, e.Code, e.Message);
            }
            catch (Exception)
            {
            }
        }

        private void WriteDiagnostic(string severity, BuildEventContext context, string project, string file, int line, int column, string code, string message)
        {
            var text = new StringBuilder("{\"t\":\"diag\",\"sev\":").Append(Json(severity));
            AppendNode(text, context);
            AppendContext(text, context);
            text.Append(",\"project\":").Append(Json(project))
                .Append(",\"file\":").Append(Json(file))
                .Append(",\"line\":").Append(line)
                .Append(",\"col\":").Append(column)
                .Append(",\"code\":").Append(Json(code))
                .Append(",\"msg\":").Append(Json(message));
            Write(text.Append('}'));
        }

        private void OnBuildFinished(object sender, BuildFinishedEventArgs e)
        {
            try
            {
                Write(new StringBuilder("{\"t\":\"end\",\"ok\":").Append(e.Succeeded ? "true" : "false").Append('}'));
            }
            catch (Exception)
            {
            }
        }

        private static void AppendNode(StringBuilder line, BuildEventContext context)
        {
            if (context != null)
            {
                line.Append(",\"node\":").Append(context.NodeId);
            }
        }

        private static void AppendContext(StringBuilder line, BuildEventContext context)
        {
            if (context != null)
            {
                line.Append(",\"ctx\":").Append(context.ProjectContextId);
            }
        }

        /// <summary>Appends "props" with the captured properties found in <paramref name="properties"/>, if any.</summary>
        private void AppendProperties(StringBuilder line, IEnumerable properties)
        {
            if (properties == null)
            {
                return;
            }

            var found = new string[CapturedProperties.Length];
            int remaining = found.Length;
            foreach (object entry in properties)
            {
                string name;
                string value;
                if (entry == null || !TryReadProperty(entry, out name, out value) || name == null)
                {
                    continue;
                }
                for (int i = 0; i < CapturedProperties.Length; i++)
                {
                    if (found[i] == null && string.Equals(name, CapturedProperties[i], StringComparison.OrdinalIgnoreCase))
                    {
                        found[i] = value ?? string.Empty;
                        remaining--;
                    }
                }
                if (remaining == 0)
                {
                    break;
                }
            }

            if (remaining == found.Length)
            {
                return;
            }

            line.Append(",\"props\":{");
            bool first = true;
            for (int i = 0; i < CapturedProperties.Length; i++)
            {
                if (found[i] == null)
                {
                    continue;
                }
                if (!first)
                {
                    line.Append(',');
                }
                first = false;
                line.Append(Json(CapturedProperties[i])).Append(':').Append(Json(found[i]));
            }
            line.Append('}');
        }

        /// <summary>
        /// Reads a name and value from a property entry. Depending on the MSBuild version and on the node the
        /// event came from, entries are DictionaryEntry, KeyValuePair, or ProjectPropertyInstance (which lives
        /// in Microsoft.Build.dll, not referenced here, so it is read through reflection).
        /// </summary>
        private bool TryReadProperty(object entry, out string name, out string value)
        {
            if (entry is DictionaryEntry)
            {
                var pair = (DictionaryEntry)entry;
                name = pair.Key as string;
                value = pair.Value as string;
                return true;
            }
            if (entry is KeyValuePair<string, string>)
            {
                var pair = (KeyValuePair<string, string>)entry;
                name = pair.Key;
                value = pair.Value;
                return true;
            }

            PropertyAccessors access;
            Type type = entry.GetType();
            lock (accessors)
            {
                if (!accessors.TryGetValue(type, out access))
                {
                    access = new PropertyAccessors(type);
                    accessors[type] = access;
                }
            }
            return access.TryRead(entry, out name, out value);
        }

        private static string GlobalProperty(IDictionary<string, string> properties, string name)
        {
            if (properties == null)
            {
                return null;
            }
            foreach (KeyValuePair<string, string> pair in properties)
            {
                if (string.Equals(pair.Key, name, StringComparison.OrdinalIgnoreCase))
                {
                    return pair.Value;
                }
            }
            return null;
        }

        private static string MsBuildVersion()
        {
            try
            {
                // The assembly version is pinned at 15.1.0.0; the file version is the real one.
                string location = typeof(IEventSource).Assembly.Location;
                string version = FileVersionInfo.GetVersionInfo(location).FileVersion;
                if (!string.IsNullOrEmpty(version))
                {
                    return version;
                }
            }
            catch (Exception)
            {
            }
            return typeof(IEventSource).Assembly.GetName().Version.ToString();
        }

        private void Write(StringBuilder line)
        {
            lock (gate)
            {
                if (writer == null)
                {
                    return;
                }
                try
                {
                    writer.WriteLine(line.ToString());
                }
                catch (Exception)
                {
                    // Disk full, file deleted, ...: stop logging, never fail the build.
                    try { writer.Dispose(); } catch (Exception) { }
                    writer = null;
                }
            }
        }

        private static string Json(string value)
        {
            if (value == null)
            {
                return "null";
            }

            var sb = new StringBuilder(value.Length + 2);
            sb.Append('"');
            foreach (char c in value)
            {
                switch (c)
                {
                    case '"': sb.Append("\\\""); break;
                    case '\\': sb.Append("\\\\"); break;
                    case '\n': sb.Append("\\n"); break;
                    case '\r': sb.Append("\\r"); break;
                    case '\t': sb.Append("\\t"); break;
                    default:
                        if (c < 0x20)
                        {
                            sb.Append("\\u").Append(((int)c).ToString("x4"));
                        }
                        else
                        {
                            sb.Append(c);
                        }
                        break;
                }
            }
            return sb.Append('"').ToString();
        }

        /// <summary>Name/value getters for one property entry type, found once and cached.</summary>
        private sealed class PropertyAccessors
        {
            private readonly PropertyInfo name;
            private readonly PropertyInfo value;

            public PropertyAccessors(Type type)
            {
                name = type.GetProperty("Name") ?? type.GetProperty("Key");
                value = type.GetProperty("EvaluatedValue") ?? type.GetProperty("Value");
            }

            public bool TryRead(object entry, out string propertyName, out string propertyValue)
            {
                propertyName = null;
                propertyValue = null;
                if (name == null || value == null)
                {
                    return false;
                }
                propertyName = name.GetValue(entry, null) as string;
                propertyValue = value.GetValue(entry, null) as string;
                return propertyName != null;
            }
        }
    }
}
